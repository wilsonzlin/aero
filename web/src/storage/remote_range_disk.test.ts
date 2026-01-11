import http from "node:http";
import type { AddressInfo } from "node:net";
import { afterEach, describe, expect, it } from "vitest";

import { assertSectorAligned, checkedOffset, SECTOR_SIZE } from "./disk";
import type {
  RemoteRangeDiskMetadataStore,
  RemoteRangeDiskSparseCache,
  RemoteRangeDiskSparseCacheFactory,
} from "./remote_range_disk";
import { RemoteRangeDisk } from "./remote_range_disk";

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
  etag?: string;
  lastModified?: string;
  ignoreRange?: boolean;
  wrongContentRange?: boolean;
  mismatchStatus?: 200 | 412;
  getBytes: (start: number, endExclusive: number) => Uint8Array;
};

type RangeServerStats = {
  rangeGets: number;
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
  const stats: RangeServerStats = { rangeGets: 0, seenIfRanges: [] };

  const server = http.createServer((req, res) => {
    const method = req.method ?? "GET";
    const range = req.headers["range"];
    const ifRange = req.headers["if-range"];

    if (typeof range === "string" && method === "GET") {
      stats.rangeGets += 1;
      stats.lastRange = range;
      if (typeof ifRange === "string") {
        stats.lastIfRange = ifRange;
        stats.seenIfRanges.push(ifRange);
      }
    }

    res.setHeader("accept-ranges", "bytes");
    if (state.etag) res.setHeader("etag", state.etag);
    if (state.lastModified) res.setHeader("last-modified", state.lastModified);

    if (method === "HEAD") {
      res.statusCode = 200;
      res.setHeader("content-length", String(state.sizeBytes));
      res.end();
      return;
    }

    if (method !== "GET") {
      res.statusCode = 405;
      res.end();
      return;
    }

    if (typeof range !== "string" || state.ignoreRange) {
      res.statusCode = 200;
      res.setHeader("content-length", String(state.sizeBytes));
      res.end(state.getBytes(0, state.sizeBytes));
      return;
    }

    const m = /^bytes=(\d+)-(\d+)$/.exec(range);
    if (!m) {
      res.statusCode = 416;
      res.end();
      return;
    }
    const start = Number(m[1]);
    const endInclusive = Number(m[2]);

    if (!Number.isSafeInteger(start) || !Number.isSafeInteger(endInclusive) || endInclusive < start) {
      res.statusCode = 416;
      res.end();
      return;
    }

    if (typeof ifRange === "string" && state.etag && ifRange !== state.etag) {
      const status = state.mismatchStatus ?? 200;
      res.statusCode = status;
      res.setHeader("content-length", String(state.sizeBytes));
      res.end(state.getBytes(0, state.sizeBytes));
      return;
    }

    if (start >= state.sizeBytes) {
      res.statusCode = 416;
      res.end();
      return;
    }

    const end = Math.min(endInclusive, state.sizeBytes - 1);
    const endExclusive = end + 1;
    const body = state.getBytes(start, endExclusive);

    res.statusCode = 206;
    if (state.wrongContentRange) {
      res.setHeader("content-range", `bytes ${start + 1}-${end}/${state.sizeBytes}`);
    } else {
      res.setHeader("content-range", `bytes ${start}-${end}/${state.sizeBytes}`);
    }
    res.setHeader("content-length", String(body.byteLength));
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

describe("RemoteRangeDisk", () => {
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
      imageKey: "test-image",
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
      imageKey: "test-image",
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
      imageKey: "test-image",
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
      imageKey: "huge-image",
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
      imageKey: "etag-image",
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

    const disk = await RemoteRangeDisk.open(server.url, {
      imageKey: "no-range",
      chunkSize,
      metadataStore: new MemoryMetadataStore(),
      sparseCacheFactory: new MemorySparseCacheFactory(),
      readAheadChunks: 0,
    });

    await expect(disk.readSectors(0, new Uint8Array(4096))).rejects.toThrow(/ignored Range/i);
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
      imageKey: "bad-content-range",
      chunkSize,
      metadataStore: new MemoryMetadataStore(),
      sparseCacheFactory: new MemorySparseCacheFactory(),
      readAheadChunks: 0,
    });

    await expect(disk.readSectors(0, new Uint8Array(4096))).rejects.toThrow(/Content-Range mismatch/i);
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
      imageKey: "clear-cache",
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
});
