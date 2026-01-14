import http from "node:http";
import type { AddressInfo } from "node:net";
import { afterEach, describe, expect, it, vi } from "vitest";

import { assertSectorAligned, checkedOffset, SECTOR_SIZE } from "./disk";
import { RANGE_STREAM_CHUNK_SIZE } from "./chunk_sizes";
import type { DiskAccessLease } from "./disk_access_lease";
import type {
  RemoteRangeDiskMetadataStore,
  RemoteRangeDiskSparseCache,
  RemoteRangeDiskSparseCacheFactory,
} from "./remote_range_disk";
import { RemoteRangeDisk } from "./remote_range_disk";
import { RemoteCacheManager, remoteRangeDeliveryType } from "./remote_cache_manager";

class MemorySparseDisk implements RemoteRangeDiskSparseCache {
  readonly sectorSize = SECTOR_SIZE;
  readonly capacityBytes: number;
  readonly blockSizeBytes: number;

  private readonly blocks = new Map<number, Uint8Array>();

  constructor(capacityBytes: number, blockSizeBytes: number) {
    this.capacityBytes = capacityBytes;
    this.blockSizeBytes = blockSizeBytes;
  }

  isBlockAllocated(blockIndex: number): boolean {
    return this.blocks.has(blockIndex);
  }

  getAllocatedBytes(): number {
    return this.blocks.size * this.blockSizeBytes;
  }

  async writeBlock(blockIndex: number, data: Uint8Array): Promise<void> {
    if (data.byteLength !== this.blockSizeBytes) {
      throw new Error("writeBlock: incorrect block size");
    }
    this.blocks.set(blockIndex, data.slice());
  }

  async readBlock(blockIndex: number, dst: Uint8Array): Promise<void> {
    if (dst.byteLength !== this.blockSizeBytes) {
      throw new Error("readBlock: incorrect block size");
    }
    dst.fill(0);
    const hit = this.blocks.get(blockIndex);
    if (hit) dst.set(hit);
  }

  async readSectors(lba: number, buffer: Uint8Array): Promise<void> {
    assertSectorAligned(buffer.byteLength, this.sectorSize);
    const offset = checkedOffset(lba, buffer.byteLength, this.sectorSize);
    if (offset + buffer.byteLength > this.capacityBytes) {
      throw new Error("read past end of disk");
    }

    let pos = 0;
    while (pos < buffer.byteLength) {
      const abs = offset + pos;
      const blockIndex = Math.floor(abs / this.blockSizeBytes);
      const within = abs % this.blockSizeBytes;
      const chunkLen = Math.min(this.blockSizeBytes - within, buffer.byteLength - pos);

      const dst = buffer.subarray(pos, pos + chunkLen);
      const hit = this.blocks.get(blockIndex);
      if (hit) {
        dst.set(hit.subarray(within, within + chunkLen));
      } else {
        dst.fill(0);
      }
      pos += chunkLen;
    }
  }

  async writeSectors(): Promise<void> {
    throw new Error("MemorySparseDisk is read-only");
  }

  async flush(): Promise<void> {}

  async close(): Promise<void> {}
}

class MemorySparseCacheFactory implements RemoteRangeDiskSparseCacheFactory {
  private readonly caches = new Map<string, MemorySparseDisk>();

  async open(cacheId: string): Promise<RemoteRangeDiskSparseCache> {
    const existing = this.caches.get(cacheId);
    if (!existing) throw new Error("cache not found");
    return existing;
  }

  async create(cacheId: string, opts: { diskSizeBytes: number; blockSizeBytes: number }): Promise<RemoteRangeDiskSparseCache> {
    const disk = new MemorySparseDisk(opts.diskSizeBytes, opts.blockSizeBytes);
    this.caches.set(cacheId, disk);
    return disk;
  }

  async delete(cacheId: string): Promise<void> {
    this.caches.delete(cacheId);
  }
}

class MemoryMetadataStore implements RemoteRangeDiskMetadataStore {
  private readonly map = new Map<string, any>();

  async read(cacheId: string): Promise<any | null> {
    return this.map.get(cacheId) ?? null;
  }

  async write(cacheId: string, meta: any): Promise<void> {
    this.map.set(cacheId, meta);
  }

  async delete(cacheId: string): Promise<void> {
    this.map.delete(cacheId);
  }
}

type RangeServerState = {
  sizeBytes: number;
  /**
   * When set, GET Range requests are validated against this size (and 416 responses include
   * `Content-Range: bytes * / <realSizeBytes>`), while HEAD continues to report `sizeBytes` until
   * updated. (The extra spaces avoid writing the comment terminator sequence.)
   *
   * This lets tests model a stale size probe / CDN drift scenario where the client believes the
   * resource is larger than it actually is.
   */
  realSizeBytes?: number;
  /**
   * When true, any range with an end beyond `realSizeBytes` returns 416 instead of truncating.
   *
   * Note: This is stricter than RFC 7233, but some servers/proxies behave this way.
   */
  return416IfEndBeyondRealSize?: boolean;
  /**
   * When true, if the server returns a 416 it will also update `sizeBytes` to `realSizeBytes`,
   * simulating a re-probe that subsequently returns the correct size.
   */
  fixHeadSizeAfter416?: boolean;
  etag?: string;
  lastModified?: string;
  headContentEncoding?: string;
  rangeContentEncoding?: string;
  requiredToken?: string;
  ignoreRange?: boolean;
  ignoreIfRangeMismatch?: boolean;
  rejectWeakIfRange?: boolean;
  wrongContentRange?: boolean;
  mismatchStatus?: 200 | 412;
  getBytes: (start: number, endExclusive: number) => Uint8Array;
};

type RangeServerStats = {
  rangeGets: number;
  headRequests: number;
  range416s: number;
  lastRange?: string;
  lastIfRange?: string;
  seenIfRanges: string[];
};

async function startRangeServer(state: RangeServerState): Promise<{
  url: string;
  state: RangeServerState;
  stats: RangeServerStats;
  close: () => Promise<void>;
}> {
  const stats: RangeServerStats = { rangeGets: 0, headRequests: 0, range416s: 0, seenIfRanges: [] };

  const server = http.createServer((req, res) => {
    if (state.requiredToken) {
      const reqUrl = req.url ?? "";
      let token: string | null = null;
      try {
        const parsed = new URL(reqUrl, "http://127.0.0.1");
        token = parsed.searchParams.get("token");
      } catch {
        token = null;
      }
      if (token !== state.requiredToken) {
        res.statusCode = 403;
        res.end();
        return;
      }
    }

    const method = req.method ?? "GET";
    const range = req.headers["range"];
    const ifRange = req.headers["if-range"];

    const isSniffRange = (() => {
      if (typeof range !== "string" || method !== "GET") return false;
      // Container sniffing may issue small header probes and a suffix tail probe.
      if (range === "bytes=-512") return true;
      const m = /^bytes=(\d+)-(\d+)$/.exec(range);
      if (!m) return false;
      const start = Number(m[1]);
      const endInclusive = Number(m[2]);
      if (!Number.isSafeInteger(start) || !Number.isSafeInteger(endInclusive) || endInclusive < start) return false;
      // Header probe: bytes=0-63 (or smaller if the file is shorter).
      if (start === 0 && endInclusive <= 63) return true;
      return false;
    })();

    if (!isSniffRange && typeof range === "string" && method === "GET") {
      stats.rangeGets += 1;
      stats.lastRange = range;
      if (typeof ifRange === "string") {
        stats.lastIfRange = ifRange;
        stats.seenIfRanges.push(ifRange);
      }
    }

    res.setHeader("accept-ranges", "bytes");
    res.setHeader("cache-control", "no-transform");
    if (state.etag) res.setHeader("etag", state.etag);
    if (state.lastModified) res.setHeader("last-modified", state.lastModified);

    if (method === "HEAD") {
      stats.headRequests += 1;
      res.statusCode = 200;
      res.setHeader("content-length", String(state.sizeBytes));
      if (state.headContentEncoding) res.setHeader("content-encoding", state.headContentEncoding);
      res.end();
      return;
    }

    if (method !== "GET") {
      res.statusCode = 405;
      res.end();
      return;
    }

    const realSizeBytes = state.realSizeBytes ?? state.sizeBytes;

    if (typeof range !== "string" || state.ignoreRange) {
      res.statusCode = 200;
      res.setHeader("content-length", String(realSizeBytes));
      res.end(state.getBytes(0, realSizeBytes));
      return;
    }

    const m = /^bytes=(\d+)-(\d+)$/.exec(range);
    const suffix = /^bytes=-(\d+)$/.exec(range);
    if (!m && !suffix) {
      res.statusCode = 416;
      res.end();
      return;
    }
    const start = m ? Number(m[1]) : Math.max(0, realSizeBytes - Number(suffix![1]));
    const endInclusive = m ? Number(m[2]) : realSizeBytes - 1;

    if (!Number.isSafeInteger(start) || !Number.isSafeInteger(endInclusive) || endInclusive < start) {
      res.statusCode = 416;
      res.end();
      return;
    }

    if (state.rejectWeakIfRange && typeof ifRange === "string") {
      const trimmed = ifRange.trimStart();
      if (trimmed.startsWith("W/") || trimmed.startsWith("w/")) {
        // Some servers reject weak validators in `If-Range` (RFC 9110 requires strong ETags).
        // Model that by falling back to a full representation.
        res.statusCode = 200;
        res.setHeader("content-length", String(state.sizeBytes));
        res.end(state.getBytes(0, state.sizeBytes));
        return;
      }
    }

    if (!state.ignoreIfRangeMismatch && typeof ifRange === "string" && (state.etag || state.lastModified)) {
      const matches =
        (state.etag !== undefined && ifRange === state.etag) ||
        (state.lastModified !== undefined && ifRange === state.lastModified);
      if (!matches) {
        const status = state.mismatchStatus ?? 200;
        res.statusCode = status;
        res.setHeader("content-length", String(state.sizeBytes));
        res.end(state.getBytes(0, state.sizeBytes));
        return;
      }
    }

    if (
      start >= realSizeBytes ||
      (state.return416IfEndBeyondRealSize === true && endInclusive >= realSizeBytes)
    ) {
      stats.range416s += 1;
      res.statusCode = 416;
      res.setHeader("content-range", `bytes */${realSizeBytes}`);
      res.end();
      if (state.fixHeadSizeAfter416 && state.realSizeBytes !== undefined) {
        state.sizeBytes = state.realSizeBytes;
      }
      return;
    }

    const end = Math.min(endInclusive, realSizeBytes - 1);
    const endExclusive = end + 1;
    const body = state.getBytes(start, endExclusive);

    res.statusCode = 206;
    if (state.wrongContentRange) {
      res.setHeader("content-range", `bytes ${start + 1}-${end}/${realSizeBytes}`);
    } else {
      res.setHeader("content-range", `bytes ${start}-${end}/${realSizeBytes}`);
    }
    res.setHeader("content-length", String(body.byteLength));
    if (!isSniffRange && state.rangeContentEncoding) res.setHeader("content-encoding", state.rangeContentEncoding);
    res.end(body);
  });

  await new Promise<void>((resolve) => {
    server.listen(0, "127.0.0.1", () => resolve());
  });
  const addr = server.address() as AddressInfo;

  return {
    url: `http://127.0.0.1:${addr.port}/image.bin`,
    state,
    stats,
    close: async () => {
      await new Promise<void>((resolve, reject) => {
        server.close((err) => (err ? reject(err) : resolve()));
      });
    },
  };
}

let activeServers: Array<() => Promise<void>> = [];

afterEach(async () => {
  const closers = activeServers;
  activeServers = [];
  for (const close of closers) await close();
});

function makeTestData(sizeBytes: number): Uint8Array {
  const out = new Uint8Array(sizeBytes);
  for (let i = 0; i < out.length; i++) out[i] = i & 0xff;
  return out;
}

function headerValue(headers: HeadersInit | undefined, name: string): string | undefined {
  if (!headers) return undefined;
  if (headers instanceof Headers) {
    return (headers.get(name) ?? headers.get(name.toLowerCase()) ?? undefined) || undefined;
  }
  const lower = name.toLowerCase();
  if (Array.isArray(headers)) {
    for (const [k, v] of headers) {
      if (k.toLowerCase() === lower) return v;
    }
    return undefined;
  }
  // Record form: `RequestInit.headers` accepts a plain object.
  const record = headers as Record<string, string>;
  return record[name] ?? record[lower] ?? undefined;
}

function parseRangeHeader(
  rangeHeader: string,
  totalSizeBytes: number,
): { start: number; endInclusive: number; isSniff: boolean } {
  const trimmed = rangeHeader.trim();
  const m = /^bytes=(\d+)-(\d+)$/.exec(trimmed);
  if (m) {
    const start = Number(m[1]);
    const endInclusive = Number(m[2]);
    if (!Number.isSafeInteger(start) || !Number.isSafeInteger(endInclusive) || start < 0 || endInclusive < start) {
      throw new Error(`invalid Range header: ${rangeHeader}`);
    }
    // Container sniffing uses a small 0-63 prefix range. Do not classify the last 512 bytes as
    // "sniff" here because a normal chunk read can legitimately target the tail of the image when
    // `chunkSize === 512`.
    const isSniff = start === 0 && endInclusive <= 63;
    return { start, endInclusive, isSniff };
  }

  const suffix = /^bytes=-(\d+)$/.exec(trimmed);
  if (suffix) {
    const suffixLen = Number(suffix[1]);
    if (!Number.isSafeInteger(suffixLen) || suffixLen <= 0) {
      throw new Error(`invalid Range header: ${rangeHeader}`);
    }
    if (!Number.isSafeInteger(totalSizeBytes) || totalSizeBytes <= 0) {
      throw new Error(`invalid totalSizeBytes ${totalSizeBytes}`);
    }
    const start = Math.max(0, totalSizeBytes - suffixLen);
    const endInclusive = totalSizeBytes - 1;
    // Suffix ranges are only used by RemoteRangeDisk's container sniffing today.
    return { start, endInclusive, isSniff: true };
  }

  throw new Error(`invalid Range header: ${rangeHeader}`);
}

describe("RemoteRangeDisk", () => {
  it("falls back to an in-memory cache when navigator.storage (OPFS) is unavailable", async () => {
    const chunkSize = 512;
    const data = makeTestData(2 * chunkSize);

    const fetchFn: typeof fetch = async (_input, init) => {
      const method = String(init?.method ?? "GET").toUpperCase();
      const rangeHeader = headerValue(init?.headers, "Range");

      if (method === "HEAD") {
        return new Response(null, {
          status: 200,
          headers: { "Content-Length": String(data.byteLength), ETag: "\"v1\"" },
        });
      }

      if (method === "GET" && typeof rangeHeader === "string") {
        const { start, endInclusive } = parseRangeHeader(rangeHeader, data.byteLength);
        const endExclusive = endInclusive + 1;
        const body = data.slice(start, endExclusive);
        return new Response(body, {
          status: 206,
          headers: {
            "Cache-Control": "no-transform",
            "Content-Range": `bytes ${start}-${endInclusive}/${data.byteLength}`,
            ETag: "\"v1\"",
          },
        });
      }

      throw new Error(`unexpected request method=${method} range=${String(rangeHeader)}`);
    };

    const nav = (globalThis as unknown as { navigator?: { storage?: unknown } }).navigator;
    const prevStorage = nav?.storage;
    if (nav) delete nav.storage;

    try {
      const disk = await RemoteRangeDisk.open("https://example.invalid/image.bin", {
        cacheKeyParts: { imageId: "no-opfs-fallback", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
        chunkSize,
        readAheadChunks: 0,
        fetchFn,
      });

      const buf = new Uint8Array(chunkSize);
      await disk.readSectors(0, buf);
      expect(buf).toEqual(data.subarray(0, buf.byteLength));
      await disk.close();
    } finally {
      if (nav) nav.storage = prevStorage;
    }
  });

  it("ignores inherited chunkSize option (prototype pollution)", async () => {
    const data = makeTestData(2 * RANGE_STREAM_CHUNK_SIZE);

    const fetchFn: typeof fetch = async (_input, init) => {
      const method = String(init?.method ?? "GET").toUpperCase();
      const rangeHeader = headerValue(init?.headers, "Range");

      if (method === "HEAD") {
        return new Response(null, {
          status: 200,
          headers: { "Content-Length": String(data.byteLength), ETag: "\"v1\"" },
        });
      }

      if (method === "GET" && typeof rangeHeader === "string") {
        const { start, endInclusive } = parseRangeHeader(rangeHeader, data.byteLength);
        const endExclusive = endInclusive + 1;
        const body = data.slice(start, endExclusive);
        return new Response(body, {
          status: 206,
          headers: {
            "Cache-Control": "no-transform",
            "Content-Range": `bytes ${start}-${endInclusive}/${data.byteLength}`,
            ETag: "\"v1\"",
          },
        });
      }

      throw new Error(`unexpected request method=${method} range=${String(rangeHeader)}`);
    };

    const chunkSizeExisting = Object.getOwnPropertyDescriptor(Object.prototype, "chunkSize");
    if (chunkSizeExisting && chunkSizeExisting.configurable === false) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    const nav = (globalThis as unknown as { navigator?: { storage?: unknown } }).navigator;
    const prevStorage = nav?.storage;
    if (nav) delete nav.storage;

    try {
      Object.defineProperty(Object.prototype, "chunkSize", {
        value: 64 * 1024 * 1024 + 512, // invalid / too large; should be ignored
        configurable: true,
        writable: true,
      });

      const disk = await RemoteRangeDisk.open("https://example.invalid/image.bin", {
        cacheKeyParts: {
          imageId: "proto-chunkSize",
          version: "v1",
          deliveryType: remoteRangeDeliveryType(RANGE_STREAM_CHUNK_SIZE),
        },
        readAheadChunks: 0,
        fetchFn,
      });

      const buf = new Uint8Array(SECTOR_SIZE);
      await disk.readSectors(0, buf);
      expect(buf).toEqual(data.subarray(0, buf.byteLength));
      await disk.close();
    } finally {
      if (chunkSizeExisting) Object.defineProperty(Object.prototype, "chunkSize", chunkSizeExisting);
      else Reflect.deleteProperty(Object.prototype, "chunkSize");

      if (nav) nav.storage = prevStorage;
    }
  });

  it("updates lastAccessedAtMs on fully cached reads (throttled)", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(0);
    try {
      const chunkSize = 512;
      const data = makeTestData(2 * chunkSize);

      let rangeGets = 0;
      const fetchFn: typeof fetch = async (_input, init) => {
        const method = String(init?.method ?? "GET").toUpperCase();
        const rangeHeader = headerValue(init?.headers, "Range");

        if (method === "HEAD") {
          return new Response(null, {
            status: 200,
            headers: { "Content-Length": String(data.byteLength), ETag: "\"v1\"" },
          });
      }

      if (method === "GET" && typeof rangeHeader === "string") {
        const parsed = parseRangeHeader(rangeHeader, data.byteLength);
        if (!parsed.isSniff) rangeGets += 1;
        const { start, endInclusive } = parsed;
        const endExclusive = endInclusive + 1;
        const body = data.slice(start, endExclusive);
        return new Response(body, {
          status: 206,
            headers: {
              "Cache-Control": "no-transform",
              "Content-Range": `bytes ${start}-${endInclusive}/${data.byteLength}`,
              ETag: "\"v1\"",
            },
          });
        }

        throw new Error(`unexpected request method=${method} range=${String(rangeHeader)}`);
      };

      class TrackingMetadataStore implements RemoteRangeDiskMetadataStore {
        private readonly map = new Map<string, any>();

        async read(cacheId: string): Promise<any | null> {
          const v = this.map.get(cacheId);
          return v ? JSON.parse(JSON.stringify(v)) : null;
        }

        async write(cacheId: string, meta: any): Promise<void> {
          this.map.set(cacheId, JSON.parse(JSON.stringify(meta)));
        }

        async delete(cacheId: string): Promise<void> {
          this.map.delete(cacheId);
        }
      }

      const metadataStore = new TrackingMetadataStore();
      const cacheKeyParts = {
        imageId: "touch-last-access",
        version: "v1",
        deliveryType: remoteRangeDeliveryType(chunkSize),
      };
      const cacheId = await RemoteCacheManager.deriveCacheKey(cacheKeyParts);

      const disk = await RemoteRangeDisk.open("https://example.invalid/image.bin", {
        cacheKeyParts,
        chunkSize,
        readAheadChunks: 0,
        metadataStore,
        sparseCacheFactory: new MemorySparseCacheFactory(),
        fetchFn,
      });

      // Prime the cache (chunk 0).
      vi.setSystemTime(1_000);
      const firstBuf = new Uint8Array(chunkSize);
      await disk.readSectors(0, firstBuf);
      expect(firstBuf).toEqual(data.subarray(0, firstBuf.byteLength));
      await disk.flush();

      const meta1 = await metadataStore.read(cacheId);
      if (!meta1) throw new Error("expected cache metadata after first read");

      // Advance beyond the touch throttle interval.
      vi.setSystemTime(meta1.lastAccessedAtMs + 61_000);
      const beforeSecond = rangeGets;

      // Cache hit: should not issue another Range GET.
      const secondBuf = new Uint8Array(chunkSize);
      await disk.readSectors(0, secondBuf);
      expect(secondBuf).toEqual(data.subarray(0, secondBuf.byteLength));
      expect(rangeGets).toBe(beforeSecond);

      await disk.close();

      const meta2 = await metadataStore.read(cacheId);
      if (!meta2) throw new Error("expected cache metadata after close");
      expect(meta2.lastAccessedAtMs).toBeGreaterThan(meta1.lastAccessedAtMs);
    } finally {
      vi.useRealTimers();
    }
  });

  it.each(["omit", "include"] as const)("passes credentials=%s through to fetchFn for HEAD + Range GET", async (credentials) => {
    const chunkSize = 512;
    const data = makeTestData(2 * chunkSize);

    const seenCredentials: Array<RequestCredentials | undefined> = [];

    const fetchFn: typeof fetch = async (_input, init) => {
      seenCredentials.push(init?.credentials as RequestCredentials | undefined);

      const method = String(init?.method ?? "GET").toUpperCase();
      const rangeHeader = headerValue(init?.headers, "Range");

      if (method === "HEAD") {
        return new Response(null, {
          status: 200,
          headers: { "Content-Length": String(data.byteLength), ETag: "\"v1\"" },
        });
      }

      if (method === "GET" && typeof rangeHeader === "string") {
        const { start, endInclusive } = parseRangeHeader(rangeHeader, data.byteLength);
        const endExclusive = endInclusive + 1;
        const body = data.slice(start, endExclusive);

        return new Response(body, {
          status: 206,
          headers: {
            "Cache-Control": "no-transform",
            "Content-Range": `bytes ${start}-${endInclusive}/${data.byteLength}`,
            ETag: "\"v1\"",
          },
        });
      }

      throw new Error(`unexpected request method=${method} range=${String(rangeHeader)}`);
    };

    const disk = await RemoteRangeDisk.open("https://example.invalid/image.bin", {
      cacheKeyParts: { imageId: "test-creds", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
      credentials,
      chunkSize,
      metadataStore: new MemoryMetadataStore(),
      sparseCacheFactory: new MemorySparseCacheFactory(),
      readAheadChunks: 0,
      fetchFn,
    });

    const buf = new Uint8Array(512);
    await disk.readSectors(0, buf);
    expect(buf).toEqual(data.subarray(0, buf.byteLength));

    // The probe HEAD request and the subsequent Range GET must both carry the requested
    // credential mode (via DiskAccessLease.credentialsMode).
    expect(seenCredentials.length).toBeGreaterThanOrEqual(2);
    expect(seenCredentials[0]).toBe(credentials);
    expect(seenCredentials[1]).toBe(credentials);

    await disk.close();
  });

  it("rejects Range responses missing Cache-Control: no-transform", async () => {
    const chunkSize = 512;
    const data = makeTestData(2 * chunkSize);
    let rangeGets = 0;
    const fetchFn: typeof fetch = async (_input, init) => {
      const method = String(init?.method ?? "GET").toUpperCase();
      const rangeHeader = headerValue(init?.headers, "Range");
      if (method === "HEAD") {
        return new Response(null, {
          status: 200,
          headers: { "Content-Length": String(data.byteLength), ETag: "\"v1\"" },
        });
      }
      if (method === "GET" && typeof rangeHeader === "string") {
        const parsed = parseRangeHeader(rangeHeader, data.byteLength);
        if (!parsed.isSniff) rangeGets += 1;
        const { start, endInclusive } = parsed;
        const endExclusive = endInclusive + 1;
        const body = data.slice(start, endExclusive);
        // Intentionally omit Cache-Control for non-sniff fetches to ensure the client fails fast
        // (anti-transform defence). Allow sniffing probes to succeed so the failure is attributed
        // to the main fetch path.
        return new Response(body, {
          status: 206,
          headers: {
            ...(parsed.isSniff ? { "Cache-Control": "no-transform" } : {}),
            "Content-Range": `bytes ${start}-${endInclusive}/${data.byteLength}`,
            ETag: "\"v1\"",
          },
        });
      }
      throw new Error(`unexpected request method=${method} range=${String(rangeHeader)}`);
    };
    const disk = await RemoteRangeDisk.open("https://example.invalid/image.bin", {
      cacheKeyParts: { imageId: "missing-no-transform", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
      chunkSize,
      metadataStore: new MemoryMetadataStore(),
      sparseCacheFactory: new MemorySparseCacheFactory(),
      readAheadChunks: 0,
      fetchFn,
    });
    await expect(disk.readSectors(0, new Uint8Array(chunkSize))).rejects.toThrow(/Cache-Control/i);
    expect(rangeGets).toBe(1);
    await disk.close().catch(() => {});
  });

  it("rejects oversized Range bodies without retrying", async () => {
    const chunkSize = 512;
    const data = makeTestData(2 * chunkSize);
    let rangeGets = 0;

    const fetchFn: typeof fetch = async (_input, init) => {
      const method = String(init?.method ?? "GET").toUpperCase();
      const rangeHeader = headerValue(init?.headers, "Range");

      if (method === "HEAD") {
        return new Response(null, {
          status: 200,
          headers: { "Content-Length": String(data.byteLength), ETag: "\"v1\"" },
        });
      }

      if (method === "GET" && typeof rangeHeader === "string") {
        const parsed = parseRangeHeader(rangeHeader, data.byteLength);
        if (!parsed.isSniff) rangeGets += 1;
        const { start, endInclusive } = parsed;
        const endExclusive = endInclusive + 1;
        const body = data.slice(start, endExclusive);
        const expectedLen = endExclusive - start;

        return new Response(body, {
          status: 206,
          headers: {
            "Cache-Control": "no-transform",
            "Content-Range": `bytes ${start}-${endInclusive}/${data.byteLength}`,
            // Deliberately lie for non-sniff fetches: indicate a larger body than requested. The
            // client must not attempt to download an arbitrarily large response.
            ...(parsed.isSniff ? {} : { "Content-Length": String(expectedLen + 1) }),
            ETag: "\"v1\"",
          },
        });
      }

      throw new Error(`unexpected request method=${method} range=${String(rangeHeader)}`);
    };

    const disk = await RemoteRangeDisk.open("https://example.invalid/image.bin", {
      cacheKeyParts: { imageId: "oversized-range-body", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
      chunkSize,
      maxRetries: 10,
      metadataStore: new MemoryMetadataStore(),
      sparseCacheFactory: new MemorySparseCacheFactory(),
      readAheadChunks: 0,
      fetchFn,
    });

    await expect(disk.readSectors(0, new Uint8Array(chunkSize))).rejects.toHaveProperty("name", "ResponseTooLargeError");
    expect(rangeGets).toBe(1);
    await disk.close();
  });

  it("limits inflight Range fetches and does not queue unbounded chunk tasks", async () => {
    const chunkSize = 512;
    const chunkCount = 100;
    const data = makeTestData(chunkSize * chunkCount);

    const maxConcurrentFetches = 4;

    let inflightGets = 0;
    let maxInflightGets = 0;
    let rangeGets = 0;
    const releaseQueue: Array<() => void> = [];

    const fetchFn: typeof fetch = async (_input, init) => {
      const method = String(init?.method ?? "GET").toUpperCase();
      const rangeHeader = headerValue(init?.headers, "Range");

      if (method === "HEAD") {
        return new Response(null, {
          status: 200,
          headers: { "Content-Length": String(data.byteLength), ETag: "\"v1\"" },
        });
      }

      if (method === "GET" && typeof rangeHeader === "string") {
        const parsed = parseRangeHeader(rangeHeader, data.byteLength);
        const { start, endInclusive } = parsed;
        const endExclusive = endInclusive + 1;
        const body = data.slice(start, endExclusive);

        // Container sniffing requests should not participate in the inflight/concurrency accounting
        // for this test.
        if (parsed.isSniff) {
          return new Response(body, {
            status: 206,
            headers: {
              "Cache-Control": "no-transform",
              "Content-Range": `bytes ${start}-${endInclusive}/${data.byteLength}`,
              ETag: "\"v1\"",
            },
          });
        }

        rangeGets += 1;
        inflightGets += 1;
        if (inflightGets > maxInflightGets) maxInflightGets = inflightGets;

        let resolve!: (resp: Response) => void;
        let reject!: (err: unknown) => void;
        const deferred = new Promise<Response>((res, rej) => {
          resolve = res;
          reject = rej;
        });

        const signal = init?.signal;
        const onAbort = () => {
          inflightGets -= 1;
          reject(new DOMException("The operation was aborted.", "AbortError"));
        };
        if (signal) {
          if (signal.aborted) {
            onAbort();
            return await deferred;
          }
          signal.addEventListener("abort", onAbort, { once: true });
        }

        const resp = new Response(body, {
          status: 206,
          headers: {
            "Cache-Control": "no-transform",
            "Content-Range": `bytes ${start}-${endInclusive}/${data.byteLength}`,
            ETag: "\"v1\"",
          },
        });

        releaseQueue.push(() => {
          if (signal) {
            try {
              signal.removeEventListener("abort", onAbort);
            } catch {
              // ignore
            }
          }
          inflightGets -= 1;
          resolve(resp);
        });

        return await deferred;
      }

      throw new Error(`unexpected request method=${method} range=${String(rangeHeader)}`);
    };

    const disk = await RemoteRangeDisk.open("https://example.invalid/image.bin", {
      cacheKeyParts: { imageId: "concurrency", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
      chunkSize,
      maxConcurrentFetches,
      maxRetries: 0,
      metadataStore: new MemoryMetadataStore(),
      sparseCacheFactory: new MemorySparseCacheFactory(),
      readAheadChunks: 0,
      fetchFn,
    });

    const buf = new Uint8Array(data.byteLength);
    const readPromise = disk.readSectors(0, buf);

    const start = Date.now();
    while (inflightGets < maxConcurrentFetches) {
      if (Date.now() - start > 5_000) {
        throw new Error("timed out waiting for initial inflight Range requests");
      }
      await new Promise((resolve) => setTimeout(resolve, 0));
    }

    // Window size should limit the number of concurrent fetch calls.
    expect(maxInflightGets).toBeLessThanOrEqual(maxConcurrentFetches);

    // And it should avoid queuing up an unbounded number of tasks waiting on the fetch semaphore.
    const semaphore = (disk as unknown as { fetchSemaphore: { waiters: unknown[] } }).fetchSemaphore;
    expect(semaphore.waiters.length).toBeLessThanOrEqual(maxConcurrentFetches);

    // Release Range responses until the read finishes.
    let done = false;
    void readPromise.finally(() => {
      done = true;
    });
    while (!done) {
      const release = releaseQueue.shift();
      if (release) {
        release();
        continue;
      }
      await new Promise((resolve) => setTimeout(resolve, 0));
    }

    await readPromise;
    expect(buf).toEqual(data);
    expect(rangeGets).toBe(chunkCount);
    await disk.close();
  });

  it("rejects chunk sizes larger than 64MiB", async () => {
    const chunkSize = 128 * 1024 * 1024;
    await expect(
      RemoteRangeDisk.open("http://example.invalid/image.bin", {
        cacheKeyParts: { imageId: "too-big-chunk", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
        chunkSize,
        metadataStore: new MemoryMetadataStore(),
        sparseCacheFactory: new MemorySparseCacheFactory(),
      }),
    ).rejects.toThrow(/chunkSize.*max/i);
  });

  it("rejects excessive readAheadChunks", async () => {
    const chunkSize = 1024 * 1024;
    await expect(
      RemoteRangeDisk.open("http://example.invalid/image.bin", {
        cacheKeyParts: { imageId: "too-big-read-ahead", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
        chunkSize,
        readAheadChunks: 1025,
        metadataStore: new MemoryMetadataStore(),
        sparseCacheFactory: new MemorySparseCacheFactory(),
      }),
    ).rejects.toThrow(/readAheadChunks.*max/i);
  });

  it("rejects excessive readAheadChunks byte volume", async () => {
    const chunkSize = 1024 * 1024; // 1 MiB
    await expect(
      RemoteRangeDisk.open("http://example.invalid/image.bin", {
        cacheKeyParts: { imageId: "too-big-read-ahead-bytes", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
        chunkSize,
        readAheadChunks: 513, // 513 MiB > 512 MiB cap
        metadataStore: new MemoryMetadataStore(),
        sparseCacheFactory: new MemorySparseCacheFactory(),
      }),
    ).rejects.toThrow(/readAhead bytes too large/i);
  });

  it("rejects excessive maxRetries", async () => {
    const chunkSize = 1024 * 1024;
    await expect(
      RemoteRangeDisk.open("http://example.invalid/image.bin", {
        cacheKeyParts: { imageId: "too-many-retries", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
        chunkSize,
        maxRetries: 33,
        metadataStore: new MemoryMetadataStore(),
        sparseCacheFactory: new MemorySparseCacheFactory(),
      }),
    ).rejects.toThrow(/maxRetries.*max/i);
  });

  it("rejects excessive maxConcurrentFetches count", async () => {
    const chunkSize = 1024 * 1024;
    await expect(
      RemoteRangeDisk.open("http://example.invalid/image.bin", {
        cacheKeyParts: { imageId: "too-many-fetches", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
        chunkSize,
        maxConcurrentFetches: 129,
        metadataStore: new MemoryMetadataStore(),
        sparseCacheFactory: new MemorySparseCacheFactory(),
      }),
    ).rejects.toThrow(/maxConcurrentFetches.*max/i);
  });

  it("rejects excessive maxConcurrentFetches byte volume", async () => {
    const chunkSize = 8 * 1024 * 1024; // 8 MiB
    await expect(
      RemoteRangeDisk.open("http://example.invalid/image.bin", {
        cacheKeyParts: { imageId: "too-many-inflight-bytes", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
        chunkSize,
        maxConcurrentFetches: 65, // 65 * 8 MiB = 520 MiB > 512 MiB cap
        metadataStore: new MemoryMetadataStore(),
        sparseCacheFactory: new MemorySparseCacheFactory(),
      }),
    ).rejects.toThrow(/inflight bytes too large/i);
  });

  it("rejects sha256Manifest length mismatch", async () => {
    const chunkSize = 1024 * 1024;
    const data = makeTestData(2 * chunkSize);
    const server = await startRangeServer({
      sizeBytes: data.byteLength,
      etag: "\"v1\"",
      getBytes: (s, e) => data.slice(s, e),
    });
    activeServers.push(server.close);

    await expect(
      RemoteRangeDisk.open(server.url, {
        cacheKeyParts: { imageId: "sha256-mismatch", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
        chunkSize,
        sha256Manifest: ["0".repeat(64)], // should be 2 chunks
        metadataStore: new MemoryMetadataStore(),
        sparseCacheFactory: new MemorySparseCacheFactory(),
      }),
    ).rejects.toThrow(/sha256Manifest length mismatch/i);
  });

  it("rejects range responses with non-identity Content-Encoding", async () => {
    const chunkSize = 1024;
    const data = makeTestData(2 * chunkSize);
    const server = await startRangeServer({
      sizeBytes: data.byteLength,
      etag: "\"v1\"",
      rangeContentEncoding: "gzip",
      getBytes: (s, e) => data.slice(s, e),
    });
    activeServers.push(server.close);

    const disk = await RemoteRangeDisk.open(server.url, {
      cacheKeyParts: { imageId: "bad-content-encoding", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
      chunkSize,
      metadataStore: new MemoryMetadataStore(),
      sparseCacheFactory: new MemorySparseCacheFactory(),
      readAheadChunks: 0,
    });

    const buf = new Uint8Array(SECTOR_SIZE);
    await expect(disk.readSectors(0, buf)).rejects.toThrow(/Content-Encoding/i);
    await disk.close();
  });

  it("single read triggers exactly one Range fetch", async () => {
    const chunkSize = 1024 * 1024;
    const data = makeTestData(2 * chunkSize);
    const server = await startRangeServer({
      sizeBytes: data.byteLength,
      etag: "\"v1\"",
      getBytes: (s, e) => data.slice(s, e),
    });
    activeServers.push(server.close);

    const disk = await RemoteRangeDisk.open(server.url, {
      cacheKeyParts: { imageId: "test-image", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
      chunkSize,
      metadataStore: new MemoryMetadataStore(),
      sparseCacheFactory: new MemorySparseCacheFactory(),
      readAheadChunks: 0,
    });

    const buf = new Uint8Array(4096);
    await disk.readSectors(0, buf);
    expect(buf).toEqual(data.subarray(0, buf.byteLength));
    expect(server.stats.rangeGets).toBe(1);
  });

  it("debounces metadata writes when caching many chunks", async () => {
    const chunkSize = 512;
    const data = makeTestData(4 * chunkSize);
    const server = await startRangeServer({
      sizeBytes: data.byteLength,
      etag: "\"v1\"",
      getBytes: (s, e) => data.slice(s, e),
    });
    activeServers.push(server.close);

    class CountingMetadataStore implements RemoteRangeDiskMetadataStore {
      writes = 0;
      private readonly map = new Map<string, any>();

      async read(cacheId: string): Promise<any | null> {
        return this.map.get(cacheId) ?? null;
      }

      async write(cacheId: string, meta: any): Promise<void> {
        this.writes += 1;
        this.map.set(cacheId, meta);
      }

      async delete(cacheId: string): Promise<void> {
        this.map.delete(cacheId);
      }
    }

    const metadataStore = new CountingMetadataStore();

    const disk = await RemoteRangeDisk.open(server.url, {
      cacheKeyParts: { imageId: "debounced-meta", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
      chunkSize,
      metadataStore,
      sparseCacheFactory: new MemorySparseCacheFactory(),
      readAheadChunks: 0,
    });

    const buf = new Uint8Array(4 * chunkSize);
    await disk.readSectors(0, buf);
    expect(buf).toEqual(data);

    // Ensure any pending debounced meta write has completed.
    await disk.flush();

    // Init + debounced updates (not per chunk). In slower environments the debounce timer can
    // fire while a multi-chunk read is still in flight, so allow up to 2 debounced updates here.
    expect(metadataStore.writes).toBeLessThanOrEqual(3);
  });

  it("exposes a telemetry snapshot compatible with the runtime disk worker", async () => {
    const chunkSize = 1024 * 1024;
    const data = makeTestData(2 * chunkSize);
    const server = await startRangeServer({
      sizeBytes: data.byteLength,
      etag: "\"v1\"",
      getBytes: (s, e) => data.slice(s, e),
    });
    activeServers.push(server.close);

    const disk = await RemoteRangeDisk.open(server.url, {
      cacheKeyParts: { imageId: "test-image", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
      chunkSize,
      metadataStore: new MemoryMetadataStore(),
      sparseCacheFactory: new MemorySparseCacheFactory(),
      readAheadChunks: 0,
    });

    const a = new Uint8Array(4096);
    await disk.readSectors(0, a);

    const first = disk.getTelemetrySnapshot();
    expect(first.totalSize).toBe(data.byteLength);
    expect(first.blockSize).toBe(chunkSize);
    expect(first.cacheLimitBytes).toBeNull();
    expect(first.blockRequests).toBe(1);
    expect(first.cacheHits).toBe(0);
    expect(first.cacheMisses).toBe(1);
    expect(first.requests).toBe(1);
    expect(first.bytesDownloaded).toBe(chunkSize);
    expect(first.cachedBytes).toBe(chunkSize);
    expect(first.inflightFetches).toBe(0);
    expect(first.lastFetchRange).toEqual({ start: 0, end: chunkSize });
    expect(first.lastFetchMs).not.toBeNull();
    expect(first.lastFetchAtMs).not.toBeNull();

    const b = new Uint8Array(4096);
    await disk.readSectors(0, b);
    const second = disk.getTelemetrySnapshot();
    expect(second.blockRequests).toBe(2);
    expect(second.cacheHits).toBe(1);
    expect(second.cacheMisses).toBe(1);
    expect(second.requests).toBe(1);
    expect(second.bytesDownloaded).toBe(chunkSize);
  });

  it("re-reading cached bytes triggers zero additional network fetches", async () => {
    const chunkSize = 1024 * 1024;
    const data = makeTestData(2 * chunkSize);
    const server = await startRangeServer({
      sizeBytes: data.byteLength,
      etag: "\"v1\"",
      getBytes: (s, e) => data.slice(s, e),
    });
    activeServers.push(server.close);

    const disk = await RemoteRangeDisk.open(server.url, {
      cacheKeyParts: { imageId: "test-image", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
      chunkSize,
      metadataStore: new MemoryMetadataStore(),
      sparseCacheFactory: new MemorySparseCacheFactory(),
      readAheadChunks: 0,
    });

    const a = new Uint8Array(4096);
    await disk.readSectors(0, a);
    expect(server.stats.rangeGets).toBe(1);

    const b = new Uint8Array(4096);
    await disk.readSectors(0, b);
    expect(server.stats.rangeGets).toBe(1);
    expect(b).toEqual(data.subarray(0, b.byteLength));
  });

  it("reports cachedBytes in remote (unpadded) bytes when the final chunk is partial", async () => {
    const chunkSize = 1024 * 1024;
    const data = makeTestData(chunkSize + 512);
    const server = await startRangeServer({
      sizeBytes: data.byteLength,
      etag: "\"v1\"",
      getBytes: (s, e) => data.slice(s, e),
    });
    activeServers.push(server.close);

    const disk = await RemoteRangeDisk.open(server.url, {
      cacheKeyParts: { imageId: "test-image", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
      chunkSize,
      metadataStore: new MemoryMetadataStore(),
      sparseCacheFactory: new MemorySparseCacheFactory(),
      readAheadChunks: 0,
    });

    // Fetch only the final partial chunk (512 bytes).
    const lastLba = chunkSize / 512;
    const tail = new Uint8Array(512);
    await disk.readSectors(lastLba, tail);
    expect(tail).toEqual(data.subarray(chunkSize, chunkSize + 512));

    const snap1 = disk.getTelemetrySnapshot();
    expect(snap1.cachedBytes).toBe(512);
    expect(snap1.cachedBytes).toBeLessThanOrEqual(snap1.totalSize);
    expect(snap1.bytesDownloaded).toBe(512);

    // Now fetch the first full chunk; cachedBytes should equal totalSize (not 2 * chunkSize).
    const head = new Uint8Array(512);
    await disk.readSectors(0, head);
    expect(head).toEqual(data.subarray(0, 512));
    const snap2 = disk.getTelemetrySnapshot();
    expect(snap2.cachedBytes).toBe(data.byteLength);
    expect(snap2.bytesDownloaded).toBe(chunkSize + 512);
  });

  it("concurrent reads to the same chunk dedupe into a single fetch", async () => {
    const chunkSize = 1024 * 1024;
    const data = makeTestData(2 * chunkSize);
    const server = await startRangeServer({
      sizeBytes: data.byteLength,
      etag: "\"v1\"",
      getBytes: (s, e) => data.slice(s, e),
    });
    activeServers.push(server.close);

    const disk = await RemoteRangeDisk.open(server.url, {
      cacheKeyParts: { imageId: "test-image", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
      chunkSize,
      metadataStore: new MemoryMetadataStore(),
      sparseCacheFactory: new MemorySparseCacheFactory(),
      readAheadChunks: 0,
    });

    const a = new Uint8Array(4096);
    const b = new Uint8Array(4096);
    await Promise.all([disk.readSectors(0, a), disk.readSectors(0, b)]);

    expect(server.stats.rangeGets).toBe(1);
    expect(a).toEqual(data.subarray(0, a.byteLength));
    expect(b).toEqual(data.subarray(0, b.byteLength));
  });

  it("handles offsets > 4GiB without truncation", async () => {
    const chunkSize = 1024 * 1024;
    const sizeBytes = 5 * 1024 * 1024 * 1024 + chunkSize;

    const server = await startRangeServer({
      sizeBytes,
      etag: "\"v1\"",
      getBytes: (start, endExclusive) => {
        const out = new Uint8Array(endExclusive - start);
        for (let i = 0; i < out.length; i++) {
          out[i] = (start + i) & 0xff;
        }
        return out;
      },
    });
    activeServers.push(server.close);

    const disk = await RemoteRangeDisk.open(server.url, {
      cacheKeyParts: { imageId: "huge-image", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
      chunkSize,
      metadataStore: new MemoryMetadataStore(),
      sparseCacheFactory: new MemorySparseCacheFactory(),
      readAheadChunks: 0,
    });

    const offsetBytes = 4 * 1024 * 1024 * 1024 + 3 * SECTOR_SIZE;
    const lba = offsetBytes / SECTOR_SIZE;
    const buf = new Uint8Array(4096);
    await disk.readSectors(lba, buf);

    const expected = new Uint8Array(buf.byteLength);
    for (let i = 0; i < expected.length; i++) expected[i] = (offsetBytes + i) & 0xff;
    expect(buf).toEqual(expected);

    expect(server.stats.lastRange).toBe(`bytes=${4 * 1024 * 1024 * 1024}-${4 * 1024 * 1024 * 1024 + chunkSize - 1}`);
  });

  it("If-Range mismatch invalidates the cache and retries successfully", async () => {
    const chunkSize = 1024 * 1024;
    let data = makeTestData(2 * chunkSize);
    let etag = "\"v1\"";

    const server = await startRangeServer({
      sizeBytes: data.byteLength,
      getBytes: (s, e) => data.slice(s, e),
      get etag() {
        return etag;
      },
      mismatchStatus: 200,
    } as RangeServerState);
    activeServers.push(server.close);

    const metadataStore = new MemoryMetadataStore();
    const sparseCacheFactory = new MemorySparseCacheFactory();

    const disk = await RemoteRangeDisk.open(server.url, {
      cacheKeyParts: { imageId: "etag-image", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
      chunkSize,
      metadataStore,
      sparseCacheFactory,
      readAheadChunks: 0,
    });

    // Cache chunk 0 under ETag v1.
    const chunk0 = new Uint8Array(4096);
    await disk.readSectors(0, chunk0);
    expect(server.stats.seenIfRanges).toContain("\"v1\"");
    expect(server.stats.rangeGets).toBe(1);

    // Mutate the server: new ETag and new content.
    data = new Uint8Array(data.length);
    data.fill(7);
    etag = "\"v2\"";

    // Read chunk 1: first attempt uses If-Range=v1 and gets a 200, forcing invalidation,
    // then retries and succeeds with If-Range=v2.
    const chunk1Lba = chunkSize / SECTOR_SIZE;
    const chunk1 = new Uint8Array(4096);
    await disk.readSectors(chunk1Lba, chunk1);
    expect(chunk1).toEqual(data.subarray(chunkSize, chunkSize + chunk1.byteLength));

    // v1 then v2 should have been observed (the retry).
    expect(server.stats.seenIfRanges).toContain("\"v1\"");
    expect(server.stats.seenIfRanges).toContain("\"v2\"");

    // Cache should have been invalidated; re-reading chunk 0 must refetch.
    const again0 = new Uint8Array(4096);
    await disk.readSectors(0, again0);
    expect(again0).toEqual(data.subarray(0, again0.byteLength));
    expect(server.stats.rangeGets).toBeGreaterThanOrEqual(4);
  });

  it("omits If-Range for weak ETags (some servers reject them)", async () => {
    const chunkSize = 1024 * 1024;
    const data = makeTestData(2 * chunkSize);
    const server = await startRangeServer({
      sizeBytes: data.byteLength,
      etag: 'W/"v1"',
      rejectWeakIfRange: true,
      getBytes: (s, e) => data.slice(s, e),
    });
    activeServers.push(server.close);

    const disk = await RemoteRangeDisk.open(server.url, {
      cacheKeyParts: { imageId: "weak-etag", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
      chunkSize,
      metadataStore: new MemoryMetadataStore(),
      sparseCacheFactory: new MemorySparseCacheFactory(),
      readAheadChunks: 0,
    });

    const buf = new Uint8Array(4096);
    await disk.readSectors(0, buf);
    expect(buf).toEqual(data.subarray(0, buf.byteLength));
    expect(server.stats.lastIfRange).toBeUndefined();
    expect(server.stats.seenIfRanges).toEqual([]);
  });

  it("uses Last-Modified for If-Range when ETag is weak", async () => {
    const chunkSize = 1024 * 1024;
    const data = makeTestData(2 * chunkSize);
    const lastModified = "Mon, 01 Jan 2024 00:00:00 GMT";
    const server = await startRangeServer({
      sizeBytes: data.byteLength,
      etag: 'W/"v1"',
      lastModified,
      rejectWeakIfRange: true,
      getBytes: (s, e) => data.slice(s, e),
    });
    activeServers.push(server.close);

    const disk = await RemoteRangeDisk.open(server.url, {
      cacheKeyParts: { imageId: "weak-etag-date", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
      chunkSize,
      metadataStore: new MemoryMetadataStore(),
      sparseCacheFactory: new MemorySparseCacheFactory(),
      readAheadChunks: 0,
    });

    const buf = new Uint8Array(4096);
    await disk.readSectors(0, buf);
    expect(buf).toEqual(data.subarray(0, buf.byteLength));
    expect(server.stats.lastIfRange).toBe(lastModified);
  });

  it("detects validator drift on 206 responses and retries successfully", async () => {
    const chunkSize = 1024 * 1024;
    let data = makeTestData(2 * chunkSize);
    let etag = "\"v1\"";

    const server = await startRangeServer({
      sizeBytes: data.byteLength,
      getBytes: (s, e) => data.slice(s, e),
      get etag() {
        return etag;
      },
      ignoreIfRangeMismatch: true,
    } as RangeServerState);
    activeServers.push(server.close);

    const disk = await RemoteRangeDisk.open(server.url, {
      cacheKeyParts: { imageId: "etag-drift", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
      chunkSize,
      metadataStore: new MemoryMetadataStore(),
      sparseCacheFactory: new MemorySparseCacheFactory(),
      readAheadChunks: 0,
    });

    // Cache chunk 0 under ETag v1.
    const chunk0 = new Uint8Array(4096);
    await disk.readSectors(0, chunk0);
    expect(server.stats.seenIfRanges).toContain("\"v1\"");
    expect(server.stats.rangeGets).toBe(1);

    // Mutate the server: new ETag and new content.
    data = new Uint8Array(data.length);
    data.fill(7);
    etag = "\"v2\"";

    // Read chunk 1: server ignores If-Range mismatch and returns 206 with the new ETag.
    // The client should detect the validator drift, invalidate, re-probe, and retry.
    const chunk1Lba = chunkSize / SECTOR_SIZE;
    const chunk1 = new Uint8Array(4096);
    await disk.readSectors(chunk1Lba, chunk1);
    expect(chunk1).toEqual(data.subarray(chunkSize, chunkSize + chunk1.byteLength));

    expect(server.stats.seenIfRanges).toContain("\"v2\"");
    expect(server.stats.rangeGets).toBeGreaterThanOrEqual(3);
  });

  it("treats 416 Range Not Satisfiable as size drift, invalidates, re-probes, and retries", async () => {
    const chunkSize = 4096;
    const realSizeBytes = chunkSize + SECTOR_SIZE;
    const reportedSizeBytes = 2 * chunkSize;
    const data = makeTestData(realSizeBytes);

    const server = await startRangeServer({
      // The initial probe (HEAD) reports a larger size than is actually available.
      sizeBytes: reportedSizeBytes,
      realSizeBytes,
      // Some servers return 416 when the requested end extends beyond the resource length.
      return416IfEndBeyondRealSize: true,
      // After serving a 416, start reporting the real size so the client's re-probe can succeed.
      fixHeadSizeAfter416: true,
      etag: "\"v1\"",
      getBytes: (s, e) => data.slice(s, e),
    });
    activeServers.push(server.close);

    const disk = await RemoteRangeDisk.open(server.url, {
      cacheKeyParts: {
        imageId: "range-416-drift",
        version: "v1",
        deliveryType: remoteRangeDeliveryType(chunkSize),
      },
      chunkSize,
      metadataStore: new MemoryMetadataStore(),
      sparseCacheFactory: new MemorySparseCacheFactory(),
      readAheadChunks: 0,
    });

    const lba = chunkSize / SECTOR_SIZE;
    const buf = new Uint8Array(SECTOR_SIZE);
    await disk.readSectors(lba, buf);
    expect(buf).toEqual(data.subarray(chunkSize, chunkSize + buf.byteLength));

    // The client should have re-probed after the 416.
    expect(server.stats.headRequests).toBe(2);
    expect(server.stats.range416s).toBe(1);
    expect(server.stats.rangeGets).toBe(2);
    expect(disk.capacityBytes).toBe(realSizeBytes);
  });

  it("rejects servers that ignore Range requests", async () => {
    const chunkSize = 1024 * 1024;
    const data = makeTestData(2 * chunkSize);
    const server = await startRangeServer({
      sizeBytes: data.byteLength,
      ignoreRange: true,
      // Do not expose ETag/Last-Modified so the client cannot treat 200 as an If-Range mismatch.
      getBytes: (s, e) => data.slice(s, e),
    });
    activeServers.push(server.close);

    await expect(
      RemoteRangeDisk.open(server.url, {
        cacheKeyParts: { imageId: "no-range", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
        chunkSize,
        metadataStore: new MemoryMetadataStore(),
        sparseCacheFactory: new MemorySparseCacheFactory(),
        readAheadChunks: 0,
      }),
    ).rejects.toThrow(/ignored Range/i);
  });

  it("rejects 206 responses with mismatched Content-Range", async () => {
    const chunkSize = 1024 * 1024;
    const data = makeTestData(2 * chunkSize);
    const server = await startRangeServer({
      sizeBytes: data.byteLength,
      wrongContentRange: true,
      etag: "\"v1\"",
      getBytes: (s, e) => data.slice(s, e),
    });
    activeServers.push(server.close);

    const disk = await RemoteRangeDisk.open(server.url, {
      cacheKeyParts: { imageId: "bad-content-range", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
      chunkSize,
      metadataStore: new MemoryMetadataStore(),
      sparseCacheFactory: new MemorySparseCacheFactory(),
      readAheadChunks: 0,
    });

    await expect(disk.readSectors(0, new Uint8Array(4096))).rejects.toThrow(/Content-Range mismatch/i);
  });

  it("rejects qcow2 images by content sniffing", async () => {
    const chunkSize = 1024;
    const data = new Uint8Array(1024);
    data.set([0x51, 0x46, 0x49, 0xfb], 0); // "QFI\xfb"
    new DataView(data.buffer).setUint32(4, 3, false);

    const server = await startRangeServer({
      sizeBytes: data.byteLength,
      etag: "\"v1\"",
      getBytes: (s, e) => data.slice(s, e),
    });
    activeServers.push(server.close);

    await expect(
      RemoteRangeDisk.open(server.url, {
        cacheKeyParts: { imageId: "qcow2-sniff", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
        chunkSize,
        metadataStore: new MemoryMetadataStore(),
        sparseCacheFactory: new MemorySparseCacheFactory(),
        readAheadChunks: 0,
      }),
    ).rejects.toThrow(/qcow2/i);
  });

  it("clearCache drops cached blocks and forces refetch", async () => {
    const chunkSize = 1024 * 1024;
    const data = makeTestData(2 * chunkSize);
    const server = await startRangeServer({
      sizeBytes: data.byteLength,
      etag: "\"v1\"",
      getBytes: (s, e) => data.slice(s, e),
    });
    activeServers.push(server.close);

    const disk = await RemoteRangeDisk.open(server.url, {
      cacheKeyParts: { imageId: "clear-cache", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
      chunkSize,
      metadataStore: new MemoryMetadataStore(),
      sparseCacheFactory: new MemorySparseCacheFactory(),
      readAheadChunks: 0,
    });

    const first = new Uint8Array(4096);
    await disk.readSectors(0, first);
    expect(first).toEqual(data.subarray(0, first.byteLength));
    expect(server.stats.rangeGets).toBe(1);

    await disk.clearCache();

    const second = new Uint8Array(4096);
    await disk.readSectors(0, second);
    expect(second).toEqual(data.subarray(0, second.byteLength));
    expect(server.stats.rangeGets).toBe(2);
  });

  it("closes cache handles if open() fails after cache creation", async () => {
    const chunkSize = 512;
    const data = makeTestData(2 * chunkSize);
    const server = await startRangeServer({
      sizeBytes: data.byteLength,
      etag: "\"v1\"",
      getBytes: (s, e) => data.slice(s, e),
    });
    activeServers.push(server.close);

    class TrackingSparseDisk extends MemorySparseDisk {
      closed = false;
      override async close(): Promise<void> {
        this.closed = true;
      }
    }

    class TrackingFactory implements RemoteRangeDiskSparseCacheFactory {
      lastCreated: TrackingSparseDisk | null = null;
      async open(_cacheId: string): Promise<RemoteRangeDiskSparseCache> {
        throw new Error("cache not found");
      }
      async create(
        _cacheId: string,
        opts: { diskSizeBytes: number; blockSizeBytes: number },
      ): Promise<RemoteRangeDiskSparseCache> {
        this.lastCreated = new TrackingSparseDisk(opts.diskSizeBytes, opts.blockSizeBytes);
        return this.lastCreated;
      }
    }

    class FailingWriteMetadataStore extends MemoryMetadataStore {
      override async write(_cacheId: string, _meta: any): Promise<void> {
        void _cacheId;
        void _meta;
        throw new Error("metadata write failed");
      }
    }

    const factory = new TrackingFactory();

    await expect(
      RemoteRangeDisk.open(server.url, {
        cacheKeyParts: { imageId: "open-failure-cleanup", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
        chunkSize,
        metadataStore: new FailingWriteMetadataStore(),
        sparseCacheFactory: factory,
      }),
    ).rejects.toThrow(/metadata write failed/i);
    expect(factory.lastCreated?.closed).toBe(true);
  });

  it("closes cache handles even if flush fails during close()", async () => {
    const chunkSize = 512;
    const data = makeTestData(2 * chunkSize);
    const server = await startRangeServer({
      sizeBytes: data.byteLength,
      etag: "\"v1\"",
      getBytes: (s, e) => data.slice(s, e),
    });
    activeServers.push(server.close);

    class FlushFailSparseDisk extends MemorySparseDisk {
      closed = false;
      override async flush(): Promise<void> {
        throw new Error("flush failed");
      }
      override async close(): Promise<void> {
        this.closed = true;
      }
    }

    class FlushFailFactory implements RemoteRangeDiskSparseCacheFactory {
      lastCreated: FlushFailSparseDisk | null = null;
      async open(_cacheId: string): Promise<RemoteRangeDiskSparseCache> {
        throw new Error("cache not found");
      }
      async create(
        _cacheId: string,
        opts: { diskSizeBytes: number; blockSizeBytes: number },
      ): Promise<RemoteRangeDiskSparseCache> {
        this.lastCreated = new FlushFailSparseDisk(opts.diskSizeBytes, opts.blockSizeBytes);
        return this.lastCreated;
      }
    }

    const factory = new FlushFailFactory();

    const disk = await RemoteRangeDisk.open(server.url, {
      cacheKeyParts: { imageId: "flush-fail-close", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
      chunkSize,
      metadataStore: new MemoryMetadataStore(),
      sparseCacheFactory: factory,
      readAheadChunks: 0,
    });

    await expect(disk.close()).rejects.toThrow(/flush failed/i);
    expect(factory.lastCreated?.closed).toBe(true);
  });

  it("treats quota errors during close() as non-fatal", async () => {
    const chunkSize = 512;
    const data = makeTestData(2 * chunkSize);
    const server = await startRangeServer({
      sizeBytes: data.byteLength,
      etag: "\"v1\"",
      getBytes: (s, e) => data.slice(s, e),
    });
    activeServers.push(server.close);

    class FlushQuotaSparseDisk extends MemorySparseDisk {
      closed = false;
      override async flush(): Promise<void> {
        throw new DOMException("quota exceeded", "QuotaExceededError");
      }
      override async close(): Promise<void> {
        this.closed = true;
      }
    }

    class FlushQuotaFactory implements RemoteRangeDiskSparseCacheFactory {
      lastCreated: FlushQuotaSparseDisk | null = null;
      async open(_cacheId: string): Promise<RemoteRangeDiskSparseCache> {
        throw new Error("cache not found");
      }
      async create(
        _cacheId: string,
        opts: { diskSizeBytes: number; blockSizeBytes: number },
      ): Promise<RemoteRangeDiskSparseCache> {
        this.lastCreated = new FlushQuotaSparseDisk(opts.diskSizeBytes, opts.blockSizeBytes);
        return this.lastCreated;
      }
    }

    const factory = new FlushQuotaFactory();

    const disk = await RemoteRangeDisk.open(server.url, {
      cacheKeyParts: { imageId: "quota-close", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
      chunkSize,
      metadataStore: new MemoryMetadataStore(),
      sparseCacheFactory: factory,
      readAheadChunks: 0,
    });

    // Read once so metadata persistence paths are exercised before close.
    const buf = new Uint8Array(chunkSize);
    await disk.readSectors(0, buf);
    expect(buf).toEqual(data.subarray(0, buf.byteLength));

    await expect(disk.close()).resolves.toBeUndefined();
    expect(factory.lastCreated?.closed).toBe(true);
  });

  it("refreshes the DiskAccessLease on 403 and retries successfully", async () => {
    const chunkSize = 1024 * 1024;
    const data = makeTestData(2 * chunkSize);
    const server = await startRangeServer({
      sizeBytes: data.byteLength,
      etag: "\"v1\"",
      requiredToken: "good",
      getBytes: (s, e) => data.slice(s, e),
    });
    activeServers.push(server.close);

    let refreshCalls = 0;
    const lease: DiskAccessLease = {
      url: `${server.url}?token=bad`,
      credentialsMode: "same-origin",
      refresh: async () => {
        refreshCalls += 1;
        lease.url = `${server.url}?token=good`;
        return lease;
      },
    };

    const disk = await RemoteRangeDisk.openWithLease(
      { sourceId: "leased-image", lease },
      {
        cacheKeyParts: { imageId: "leased-image", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
        chunkSize,
        metadataStore: new MemoryMetadataStore(),
        sparseCacheFactory: new MemorySparseCacheFactory(),
        readAheadChunks: 0,
      },
    );

    const buf = new Uint8Array(4096);
    await disk.readSectors(0, buf);
    expect(buf).toEqual(data.subarray(0, buf.byteLength));
    expect(refreshCalls).toBe(1);
  });

  it("aborts inflight read-ahead fetches and closes cleanly", async () => {
    const chunkSize = 512;
    const data = makeTestData(chunkSize * 4);

    let blockedStartedResolve: (() => void) | null = null;
    const blockedStarted = new Promise<void>((resolve) => {
      blockedStartedResolve = resolve;
    });
    // Use a no-op default so TypeScript doesn't treat this as permanently `null`
    // (it is reassigned inside the stub fetcher when the read-ahead request is created).
    let releaseBlockedFetch: () => void = () => {};
    let writeAfterClose = false;

    class TrackingSparseDisk extends MemorySparseDisk {
      closed = false;
      override async writeBlock(blockIndex: number, bytes: Uint8Array): Promise<void> {
        if (this.closed) writeAfterClose = true;
        return await super.writeBlock(blockIndex, bytes);
      }
      override async close(): Promise<void> {
        this.closed = true;
      }
    }

    class TrackingFactory implements RemoteRangeDiskSparseCacheFactory {
      lastCreated: TrackingSparseDisk | null = null;
      async open(_cacheId: string): Promise<RemoteRangeDiskSparseCache> {
        throw new Error("cache not found");
      }
      async create(
        _cacheId: string,
        opts: { diskSizeBytes: number; blockSizeBytes: number },
      ): Promise<RemoteRangeDiskSparseCache> {
        this.lastCreated = new TrackingSparseDisk(opts.diskSizeBytes, opts.blockSizeBytes);
        return this.lastCreated;
      }
    }

    const toArrayBuffer = (bytes: Uint8Array): ArrayBuffer => {
      const buf = new ArrayBuffer(bytes.byteLength);
      new Uint8Array(buf).set(bytes);
      return buf;
    };

    const fetcher: typeof fetch = async (_url, init) => {
      const method = String(init?.method || "GET").toUpperCase();
      const rangeHeader = headerValue(init?.headers, "Range");

      if (method === "HEAD") {
        return new Response(null, {
          status: 200,
          headers: { "Content-Length": String(data.byteLength), ETag: "\"v1\"" },
        });
      }

      if (method !== "GET") {
        return new Response(null, { status: 405 });
      }

      if (!rangeHeader) {
        return new Response(toArrayBuffer(data), { status: 200, headers: { "Content-Length": String(data.byteLength) } });
      }

      const { start, endInclusive } = parseRangeHeader(rangeHeader, data.byteLength);
      const slice = data.subarray(start, Math.min(endInclusive + 1, data.byteLength));
      const body = toArrayBuffer(slice);
      const resp = new Response(body, {
        status: 206,
        headers: {
          "Cache-Control": "no-transform",
          "Content-Range": `bytes ${start}-${start + body.byteLength - 1}/${data.byteLength}`,
          ETag: "\"v1\"",
        },
      });

      // Block the first read-ahead chunk (chunk 2).
      if (start === chunkSize * 2) {
        blockedStartedResolve?.();
        blockedStartedResolve = null;

        return await new Promise<Response>((resolve, reject) => {
          let settled = false;
          const signal = init?.signal;

          const abortErr = () => {
            const e = new Error("aborted");
            e.name = "AbortError";
            return e;
          };

          const onAbort = () => {
            if (settled) return;
            settled = true;
            releaseBlockedFetch = () => {};
            reject(abortErr());
          };

          if (signal?.aborted) {
            onAbort();
            return;
          }

          signal?.addEventListener("abort", onAbort, { once: true });

          releaseBlockedFetch = () => {
            if (settled) return;
            settled = true;
            signal?.removeEventListener("abort", onAbort);
            resolve(resp);
          };
        });
      }

      return resp;
    };

    const factory = new TrackingFactory();
    const disk = await RemoteRangeDisk.open("https://example.invalid/disk.img", {
      cacheKeyParts: {
        imageId: "prefetch-close-race",
        version: "v1",
        deliveryType: remoteRangeDeliveryType(chunkSize),
      },
      chunkSize,
      readAheadChunks: 1,
      metadataStore: new MemoryMetadataStore(),
      sparseCacheFactory: factory,
      fetchFn: fetcher,
    });

    // First read sets the sequential baseline.
    await disk.readSectors(0, new Uint8Array(chunkSize));
    // Second sequential read triggers read-ahead.
    await disk.readSectors(1, new Uint8Array(chunkSize));
    await blockedStarted;

    const unhandled: unknown[] = [];
    const onUnhandled = (reason: unknown) => {
      unhandled.push(reason);
    };
    process.on("unhandledRejection", onUnhandled);
    try {
      const closePromise = disk.close();
      releaseBlockedFetch();
      await closePromise;

      // Give any remaining microtasks a chance to run.
      await new Promise((resolve) => setTimeout(resolve, 0));

      expect(factory.lastCreated?.closed).toBe(true);
      expect(unhandled).toEqual([]);
      expect(writeAfterClose).toBe(false);
    } finally {
      process.off("unhandledRejection", onUnhandled);
    }
  });

  it("drops persistent cache writes on quota errors without breaking reads", async () => {
    const chunkSize = 512;
    const data = makeTestData(chunkSize * 4);
    let rangeGets = 0;

    const fetchFn: typeof fetch = async (_input, init) => {
      const method = String(init?.method ?? "GET").toUpperCase();
      const rangeHeader = headerValue(init?.headers, "Range");

      if (method === "HEAD") {
        return new Response(null, {
          status: 200,
          headers: { "Content-Length": String(data.byteLength), ETag: "\"v1\"" },
        });
      }

      if (method === "GET" && typeof rangeHeader === "string") {
        const parsed = parseRangeHeader(rangeHeader, data.byteLength);
        if (!parsed.isSniff) rangeGets += 1;
        const { start, endInclusive } = parsed;
        const endExclusive = endInclusive + 1;
        const body = data.slice(start, endExclusive);

        return new Response(body, {
          status: 206,
          headers: {
            "Cache-Control": "no-transform",
            "Content-Range": `bytes ${start}-${endInclusive}/${data.byteLength}`,
            ETag: "\"v1\"",
          },
        });
      }

      throw new Error(`unexpected request method=${method} range=${String(rangeHeader)}`);
    };

    class QuotaFailDisk extends MemorySparseDisk {
      writeCalls = 0;

      override async writeBlock(_blockIndex: number, _data: Uint8Array): Promise<void> {
        this.writeCalls += 1;
        throw new DOMException("quota exceeded", "QuotaExceededError");
      }
    }

    class QuotaFailFactory implements RemoteRangeDiskSparseCacheFactory {
      lastCreated: QuotaFailDisk | null = null;

      async open(_cacheId: string): Promise<RemoteRangeDiskSparseCache> {
        throw new Error("cache not found");
      }

      async create(
        _cacheId: string,
        opts: { diskSizeBytes: number; blockSizeBytes: number },
      ): Promise<RemoteRangeDiskSparseCache> {
        this.lastCreated = new QuotaFailDisk(opts.diskSizeBytes, opts.blockSizeBytes);
        return this.lastCreated;
      }
    }

    const factory = new QuotaFailFactory();
    const disk = await RemoteRangeDisk.open("https://example.invalid/image.bin", {
      cacheKeyParts: { imageId: "quota-drop", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
      chunkSize,
      // Ensure the quota error happens "mid-read spanning many chunks" while still being deterministic.
      maxConcurrentFetches: 1,
      readAheadChunks: 0,
      metadataStore: new MemoryMetadataStore(),
      sparseCacheFactory: factory,
      fetchFn,
    });

    const buf = new Uint8Array(chunkSize * 3);
    await disk.readSectors(0, buf);
    expect(buf).toEqual(data.subarray(0, buf.byteLength));

    // After the first quota error, RemoteRangeDisk should stop trying to persist further chunks.
    expect(factory.lastCreated?.writeCalls).toBe(1);

    const snapshot = disk.getTelemetrySnapshot();
    expect(snapshot.cacheLimitBytes).toBe(0);
    expect(rangeGets).toBe(3);

    await disk.close();
  });

  it("drops persistent cache writes on Firefox quota errors (NS_ERROR_DOM_QUOTA_REACHED) without breaking reads", async () => {
    const chunkSize = 512;
    const data = makeTestData(chunkSize * 4);
    let rangeGets = 0;

    const fetchFn: typeof fetch = async (_input, init) => {
      const method = String(init?.method ?? "GET").toUpperCase();
      const rangeHeader = headerValue(init?.headers, "Range");

      if (method === "HEAD") {
        return new Response(null, {
          status: 200,
          headers: { "Content-Length": String(data.byteLength), ETag: "\"v1\"" },
        });
      }

      if (method === "GET" && typeof rangeHeader === "string") {
        const parsed = parseRangeHeader(rangeHeader, data.byteLength);
        if (!parsed.isSniff) rangeGets += 1;
        const { start, endInclusive } = parsed;
        const endExclusive = endInclusive + 1;
        const body = data.slice(start, endExclusive);

        return new Response(body, {
          status: 206,
          headers: {
            "Cache-Control": "no-transform",
            "Content-Range": `bytes ${start}-${endInclusive}/${data.byteLength}`,
            ETag: "\"v1\"",
          },
        });
      }

      throw new Error(`unexpected request method=${method} range=${String(rangeHeader)}`);
    };

    class FirefoxQuotaFailDisk extends MemorySparseDisk {
      writeCalls = 0;

      override async writeBlock(_blockIndex: number, _data: Uint8Array): Promise<void> {
        this.writeCalls += 1;
        throw new DOMException("quota exceeded", "NS_ERROR_DOM_QUOTA_REACHED");
      }
    }

    class FirefoxQuotaFailFactory implements RemoteRangeDiskSparseCacheFactory {
      lastCreated: FirefoxQuotaFailDisk | null = null;

      async open(_cacheId: string): Promise<RemoteRangeDiskSparseCache> {
        throw new Error("cache not found");
      }

      async create(
        _cacheId: string,
        opts: { diskSizeBytes: number; blockSizeBytes: number },
      ): Promise<RemoteRangeDiskSparseCache> {
        this.lastCreated = new FirefoxQuotaFailDisk(opts.diskSizeBytes, opts.blockSizeBytes);
        return this.lastCreated;
      }
    }

    const factory = new FirefoxQuotaFailFactory();
    const disk = await RemoteRangeDisk.open("https://example.invalid/image.bin", {
      cacheKeyParts: { imageId: "quota-drop-firefox", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
      chunkSize,
      maxConcurrentFetches: 1,
      readAheadChunks: 0,
      metadataStore: new MemoryMetadataStore(),
      sparseCacheFactory: factory,
      fetchFn,
    });

    const buf = new Uint8Array(chunkSize * 3);
    await expect(disk.readSectors(0, buf)).resolves.toBeUndefined();
    expect(buf).toEqual(data.subarray(0, buf.byteLength));

    expect(factory.lastCreated?.writeCalls).toBe(1);
    expect(disk.getTelemetrySnapshot().cacheLimitBytes).toBe(0);
    expect(rangeGets).toBe(3);

    await disk.close();
  });

  it("disables persistent caching if metadata persistence hits quota (and continues serving reads)", async () => {
    const chunkSize = 512;
    const data = makeTestData(chunkSize * 2);
    let rangeGets = 0;

    const fetchFn: typeof fetch = async (_input, init) => {
      const method = String(init?.method ?? "GET").toUpperCase();
      const rangeHeader = headerValue(init?.headers, "Range");

      if (method === "HEAD") {
        return new Response(null, {
          status: 200,
          headers: { "Content-Length": String(data.byteLength), ETag: "\"v1\"" },
        });
      }

      if (method === "GET" && typeof rangeHeader === "string") {
        const parsed = parseRangeHeader(rangeHeader, data.byteLength);
        if (!parsed.isSniff) rangeGets += 1;
        const { start, endInclusive } = parsed;
        const endExclusive = endInclusive + 1;
        const body = data.slice(start, endExclusive);

        return new Response(body, {
          status: 206,
          headers: {
            "Cache-Control": "no-transform",
            "Content-Range": `bytes ${start}-${endInclusive}/${data.byteLength}`,
            ETag: "\"v1\"",
          },
        });
      }

      throw new Error(`unexpected request method=${method} range=${String(rangeHeader)}`);
    };

    class TrackingDisk extends MemorySparseDisk {
      writeCalls = 0;
      override async writeBlock(blockIndex: number, bytes: Uint8Array): Promise<void> {
        this.writeCalls += 1;
        await super.writeBlock(blockIndex, bytes);
      }
    }

    class TrackingFactory implements RemoteRangeDiskSparseCacheFactory {
      lastCreated: TrackingDisk | null = null;

      async open(_cacheId: string): Promise<RemoteRangeDiskSparseCache> {
        throw new Error("cache not found");
      }

      async create(
        _cacheId: string,
        opts: { diskSizeBytes: number; blockSizeBytes: number },
      ): Promise<RemoteRangeDiskSparseCache> {
        this.lastCreated = new TrackingDisk(opts.diskSizeBytes, opts.blockSizeBytes);
        return this.lastCreated;
      }
    }

    class QuotaFailMetadataStore extends MemoryMetadataStore {
      writes = 0;
      override async write(cacheId: string, meta: any): Promise<void> {
        this.writes += 1;
        // Allow initial metadata write during init, then simulate quota exhaustion.
        if (this.writes >= 2) {
          throw new DOMException("quota exceeded", "QuotaExceededError");
        }
        await super.write(cacheId, meta);
      }
    }

    const factory = new TrackingFactory();
    const metadataStore = new QuotaFailMetadataStore();
    const disk = await RemoteRangeDisk.open("https://example.invalid/image.bin", {
      cacheKeyParts: { imageId: "quota-drop-meta", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
      chunkSize,
      maxConcurrentFetches: 1,
      readAheadChunks: 0,
      metadataStore,
      sparseCacheFactory: factory,
      fetchFn,
    });

    const first = new Uint8Array(chunkSize);
    await disk.readSectors(0, first);
    expect(first).toEqual(data.subarray(0, first.byteLength));
    expect(rangeGets).toBe(1);
    expect(factory.lastCreated?.writeCalls).toBe(1);

    // Force metadata persistence; it should fail with quota and disable persistent caching.
    await disk.flush();
    expect(disk.getTelemetrySnapshot().cacheLimitBytes).toBe(0);
    expect(metadataStore.writes).toBe(2);

    // With caching disabled, subsequent reads should re-download and must not attempt further
    // sparse cache writes.
    const second = new Uint8Array(chunkSize);
    await disk.readSectors(0, second);
    expect(second).toEqual(data.subarray(0, second.byteLength));
    expect(rangeGets).toBe(2);
    expect(factory.lastCreated?.writeCalls).toBe(1);
    expect(metadataStore.writes).toBe(2);

    await disk.close();
  });

  it("treats clearCache quota failures as non-fatal (cache disabled)", async () => {
    const chunkSize = 512;
    const data = makeTestData(chunkSize * 2);
    let rangeGets = 0;

    const fetchFn: typeof fetch = async (_input, init) => {
      const method = String(init?.method ?? "GET").toUpperCase();
      const rangeHeader = headerValue(init?.headers, "Range");

      if (method === "HEAD") {
        return new Response(null, {
          status: 200,
          headers: { "Content-Length": String(data.byteLength), ETag: "\"v1\"" },
        });
      }

      if (method === "GET" && typeof rangeHeader === "string") {
        const parsed = parseRangeHeader(rangeHeader, data.byteLength);
        if (!parsed.isSniff) rangeGets += 1;
        const { start, endInclusive } = parsed;
        const endExclusive = endInclusive + 1;
        const body = data.slice(start, endExclusive);

        return new Response(body, {
          status: 206,
          headers: {
            "Cache-Control": "no-transform",
            "Content-Range": `bytes ${start}-${endInclusive}/${data.byteLength}`,
            ETag: "\"v1\"",
          },
        });
      }

      throw new Error(`unexpected request method=${method} range=${String(rangeHeader)}`);
    };

    class FlakyCreateFactory implements RemoteRangeDiskSparseCacheFactory {
      createCalls = 0;

      async open(_cacheId: string): Promise<RemoteRangeDiskSparseCache> {
        throw new Error("cache not found");
      }

      async create(
        _cacheId: string,
        opts: { diskSizeBytes: number; blockSizeBytes: number },
      ): Promise<RemoteRangeDiskSparseCache> {
        this.createCalls += 1;
        if (this.createCalls >= 2) {
          throw new DOMException("quota exceeded", "QuotaExceededError");
        }
        return new MemorySparseDisk(opts.diskSizeBytes, opts.blockSizeBytes);
      }
    }

    const factory = new FlakyCreateFactory();
    const disk = await RemoteRangeDisk.open("https://example.invalid/image.bin", {
      cacheKeyParts: { imageId: "quota-drop-clear-cache", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
      chunkSize,
      maxConcurrentFetches: 1,
      readAheadChunks: 0,
      metadataStore: new MemoryMetadataStore(),
      sparseCacheFactory: factory,
      fetchFn,
    });

    const first = new Uint8Array(chunkSize);
    await disk.readSectors(0, first);
    expect(first).toEqual(data.subarray(0, first.byteLength));
    expect(rangeGets).toBe(1);

    await expect(disk.clearCache()).resolves.toBeUndefined();
    expect(disk.getTelemetrySnapshot().cacheLimitBytes).toBe(0);

    // With persistence disabled, the next read must hit the network again.
    const second = new Uint8Array(chunkSize);
    await disk.readSectors(0, second);
    expect(second).toEqual(data.subarray(0, second.byteLength));
    expect(rangeGets).toBe(2);

    await disk.close();
  });

  it("drops persistent caching if sparse cache readSectors hits quota (and continues serving reads)", async () => {
    const chunkSize = 512;
    const data = makeTestData(chunkSize * 2);
    let rangeGets = 0;

    const fetchFn: typeof fetch = async (_input, init) => {
      const method = String(init?.method ?? "GET").toUpperCase();
      const rangeHeader = headerValue(init?.headers, "Range");

      if (method === "HEAD") {
        return new Response(null, {
          status: 200,
          headers: { "Content-Length": String(data.byteLength), ETag: "\"v1\"" },
        });
      }

      if (method === "GET" && typeof rangeHeader === "string") {
        const parsed = parseRangeHeader(rangeHeader, data.byteLength);
        if (!parsed.isSniff) rangeGets += 1;
        const { start, endInclusive } = parsed;
        const endExclusive = endInclusive + 1;
        const body = data.slice(start, endExclusive);

        return new Response(body, {
          status: 206,
          headers: {
            "Cache-Control": "no-transform",
            "Content-Range": `bytes ${start}-${endInclusive}/${data.byteLength}`,
            ETag: "\"v1\"",
          },
        });
      }

      throw new Error(`unexpected request method=${method} range=${String(rangeHeader)}`);
    };

    class ReadQuotaDisk extends MemorySparseDisk {
      writeCalls = 0;
      readCalls = 0;

      override async writeBlock(blockIndex: number, bytes: Uint8Array): Promise<void> {
        this.writeCalls += 1;
        await super.writeBlock(blockIndex, bytes);
      }

      override async readSectors(_lba: number, _buffer: Uint8Array): Promise<void> {
        this.readCalls += 1;
        throw new DOMException("quota exceeded", "QuotaExceededError");
      }
    }

    class ReadQuotaFactory implements RemoteRangeDiskSparseCacheFactory {
      lastCreated: ReadQuotaDisk | null = null;

      async open(_cacheId: string): Promise<RemoteRangeDiskSparseCache> {
        throw new Error("cache not found");
      }

      async create(
        _cacheId: string,
        opts: { diskSizeBytes: number; blockSizeBytes: number },
      ): Promise<RemoteRangeDiskSparseCache> {
        this.lastCreated = new ReadQuotaDisk(opts.diskSizeBytes, opts.blockSizeBytes);
        return this.lastCreated;
      }
    }

    const factory = new ReadQuotaFactory();
    const disk = await RemoteRangeDisk.open("https://example.invalid/image.bin", {
      cacheKeyParts: { imageId: "quota-drop-read", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
      chunkSize,
      maxConcurrentFetches: 1,
      readAheadChunks: 0,
      metadataStore: new MemoryMetadataStore(),
      sparseCacheFactory: factory,
      fetchFn,
    });

    const buf = new Uint8Array(chunkSize * 2);
    await expect(disk.readSectors(0, buf)).resolves.toBeUndefined();
    expect(buf).toEqual(data.subarray(0, buf.byteLength));

    // First attempt downloads+persists, then the sparse cache read path hits quota and forces a
    // retry in memory/network mode (re-download).
    expect(factory.lastCreated?.writeCalls).toBe(2);
    expect(factory.lastCreated?.readCalls).toBe(1);
    expect(rangeGets).toBe(4);
    expect(disk.getTelemetrySnapshot().cacheLimitBytes).toBe(0);

    // Once in quota-disabled mode, subsequent reads should not touch the sparse cache and should
    // re-download (network-only) instead of retaining an unbounded in-memory cache.
    const before = rangeGets;
    const again = new Uint8Array(chunkSize * 2);
    await disk.readSectors(0, again);
    expect(again).toEqual(data.subarray(0, again.byteLength));
    expect(rangeGets).toBe(before + 2);
    expect(factory.lastCreated?.readCalls).toBe(1);

    await disk.close();
  });

  it("handles quota errors mid-read after some chunks were cached and restarts safely", async () => {
    const chunkSize = 512;
    const data = makeTestData(chunkSize * 4);
    let rangeGets = 0;

    const fetchFn: typeof fetch = async (_input, init) => {
      const method = String(init?.method ?? "GET").toUpperCase();
      const rangeHeader = headerValue(init?.headers, "Range");

      if (method === "HEAD") {
        return new Response(null, {
          status: 200,
          headers: { "Content-Length": String(data.byteLength), ETag: "\"v1\"" },
        });
      }

      if (method === "GET" && typeof rangeHeader === "string") {
        const parsed = parseRangeHeader(rangeHeader, data.byteLength);
        if (!parsed.isSniff) rangeGets += 1;
        const { start, endInclusive } = parsed;
        const endExclusive = endInclusive + 1;
        const body = data.slice(start, endExclusive);

        return new Response(body, {
          status: 206,
          headers: {
            "Cache-Control": "no-transform",
            "Content-Range": `bytes ${start}-${endInclusive}/${data.byteLength}`,
            ETag: "\"v1\"",
          },
        });
      }

      throw new Error(`unexpected request method=${method} range=${String(rangeHeader)}`);
    };

    class FlakyQuotaDisk extends MemorySparseDisk {
      writeCalls = 0;
      override async writeBlock(blockIndex: number, bytes: Uint8Array): Promise<void> {
        this.writeCalls += 1;
        // Allow the first chunk to be written, then simulate quota exhaustion.
        if (this.writeCalls >= 2) {
          throw new DOMException("quota exceeded", "QuotaExceededError");
        }
        await super.writeBlock(blockIndex, bytes);
      }
    }

    class FlakyQuotaFactory implements RemoteRangeDiskSparseCacheFactory {
      lastCreated: FlakyQuotaDisk | null = null;

      async open(_cacheId: string): Promise<RemoteRangeDiskSparseCache> {
        throw new Error("cache not found");
      }

      async create(
        _cacheId: string,
        opts: { diskSizeBytes: number; blockSizeBytes: number },
      ): Promise<RemoteRangeDiskSparseCache> {
        this.lastCreated = new FlakyQuotaDisk(opts.diskSizeBytes, opts.blockSizeBytes);
        return this.lastCreated;
      }
    }

    const factory = new FlakyQuotaFactory();
    const disk = await RemoteRangeDisk.open("https://example.invalid/image.bin", {
      cacheKeyParts: { imageId: "quota-drop-mid-read", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
      chunkSize,
      // Deterministic ordering: chunk 0 caches successfully, chunk 1 hits quota.
      maxConcurrentFetches: 1,
      readAheadChunks: 0,
      metadataStore: new MemoryMetadataStore(),
      sparseCacheFactory: factory,
      fetchFn,
    });

    const buf = new Uint8Array(chunkSize * 3);
    await disk.readSectors(0, buf);
    expect(buf).toEqual(data.subarray(0, buf.byteLength));

    // One successful write, then a quota error; no further persistent writes should be attempted.
    expect(factory.lastCreated?.writeCalls).toBe(2);

    const snapshot = disk.getTelemetrySnapshot();
    expect(snapshot.cacheLimitBytes).toBe(0);
    expect(snapshot.cachedBytes).toBe(0);

    // The read spans 3 chunks. We expect an initial pass (3 fetches) and a restart that re-fetches
    // chunk 0 once persistence is disabled.
    expect(rangeGets).toBe(4);

    await disk.close();
  });

  it("disables persistent caching if background flush hits quota (and continues serving reads)", async () => {
    const chunkSize = 512;
    const data = makeTestData(chunkSize * 2);
    let rangeGets = 0;

    const fetchFn: typeof fetch = async (_input, init) => {
      const method = String(init?.method ?? "GET").toUpperCase();
      const rangeHeader = headerValue(init?.headers, "Range");

      if (method === "HEAD") {
        return new Response(null, {
          status: 200,
          headers: { "Content-Length": String(data.byteLength), ETag: "\"v1\"" },
        });
      }

      if (method === "GET" && typeof rangeHeader === "string") {
        const parsed = parseRangeHeader(rangeHeader, data.byteLength);
        if (!parsed.isSniff) rangeGets += 1;
        const { start, endInclusive } = parsed;
        const endExclusive = endInclusive + 1;
        const body = data.slice(start, endExclusive);

        return new Response(body, {
          status: 206,
          headers: {
            "Cache-Control": "no-transform",
            "Content-Range": `bytes ${start}-${endInclusive}/${data.byteLength}`,
            ETag: "\"v1\"",
          },
        });
      }

      throw new Error(`unexpected request method=${method} range=${String(rangeHeader)}`);
    };

    class FlushQuotaDisk extends MemorySparseDisk {
      writeCalls = 0;
      flushCalls = 0;

      override async writeBlock(blockIndex: number, bytes: Uint8Array): Promise<void> {
        this.writeCalls += 1;
        await super.writeBlock(blockIndex, bytes);
      }

      override async flush(): Promise<void> {
        this.flushCalls += 1;
        // Fail the first background flush with quota, but allow subsequent flushes
        // (e.g. during close()) to succeed.
        if (this.flushCalls === 1) {
          throw new DOMException("quota exceeded", "QuotaExceededError");
        }
      }
    }

    class FlushQuotaFactory implements RemoteRangeDiskSparseCacheFactory {
      lastCreated: FlushQuotaDisk | null = null;

      async open(_cacheId: string): Promise<RemoteRangeDiskSparseCache> {
        throw new Error("cache not found");
      }

      async create(
        _cacheId: string,
        opts: { diskSizeBytes: number; blockSizeBytes: number },
      ): Promise<RemoteRangeDiskSparseCache> {
        this.lastCreated = new FlushQuotaDisk(opts.diskSizeBytes, opts.blockSizeBytes);
        return this.lastCreated;
      }
    }

    const factory = new FlushQuotaFactory();
    const disk = await RemoteRangeDisk.open("https://example.invalid/image.bin", {
      cacheKeyParts: { imageId: "quota-drop-flush", version: "v1", deliveryType: remoteRangeDeliveryType(chunkSize) },
      chunkSize,
      maxConcurrentFetches: 1,
      readAheadChunks: 0,
      metadataStore: new MemoryMetadataStore(),
      sparseCacheFactory: factory,
      fetchFn,
    });

    const first = new Uint8Array(chunkSize);
    await disk.readSectors(0, first);
    expect(first).toEqual(data.subarray(0, first.byteLength));
    expect(rangeGets).toBe(1);

    // Allow the scheduled background flush to run and observe quota exhaustion.
    await new Promise((resolve) => setTimeout(resolve, 0));
    await new Promise((resolve) => setTimeout(resolve, 0));

    const snap = disk.getTelemetrySnapshot();
    expect(snap.cacheLimitBytes).toBe(0);
    expect(snap.cachedBytes).toBe(0);
    expect(factory.lastCreated?.flushCalls).toBeGreaterThanOrEqual(1);

    // With persistence disabled, subsequent reads should re-download.
    const second = new Uint8Array(chunkSize);
    await disk.readSectors(0, second);
    expect(second).toEqual(data.subarray(0, second.byteLength));
    expect(rangeGets).toBe(2);

    // Persistent writes should not be attempted again once disabled.
    expect(factory.lastCreated?.writeCalls).toBe(1);

    await disk.close();
  });
});
