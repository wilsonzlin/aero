import { afterEach, describe, expect, it, vi } from "vitest";

import { RemoteStreamingDisk, stableCacheKey } from "./remote_disk";
import { remoteRangeDeliveryType, RemoteCacheManager } from "../storage/remote_cache_manager";
import { getDir, installMemoryOpfs, MemoryDirectoryHandle, MemoryFileHandle } from "../test_utils/memory_opfs";

function makeTestImage(size: number): Uint8Array {
  const buf = new Uint8Array(size);
  for (let i = 0; i < size; i += 1) buf[i] = (i * 13) & 0xff;
  return buf;
}

type FetchStats = {
  totalCalls: number;
  probeRangeCalls: number;
  chunkRangeCalls: number;
};

function installMockRangeFetch(data: Uint8Array, opts: { etag: string }): { stats: FetchStats; restore: () => void } {
  const original = globalThis.fetch;
  const stats: FetchStats = { totalCalls: 0, probeRangeCalls: 0, chunkRangeCalls: 0 };

  function headerValue(init: RequestInit | undefined, name: string): string | null {
    const h = init?.headers;
    if (!h) return null;
    if (h instanceof Headers) return h.get(name);
    if (Array.isArray(h)) {
      for (const [k, v] of h) {
        if (k.toLowerCase() === name.toLowerCase()) return v;
      }
      return null;
    }
    const rec = h as Record<string, string>;
    for (const [k, v] of Object.entries(rec)) {
      if (k.toLowerCase() === name.toLowerCase()) return v;
    }
    return null;
  }

  globalThis.fetch = (async (_input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    stats.totalCalls += 1;
    const method = (init?.method ?? "GET").toUpperCase();

    if (method === "HEAD") {
      return new Response(null, {
        status: 200,
        headers: {
          "Content-Length": String(data.byteLength),
          "Accept-Ranges": "bytes",
          ETag: opts.etag,
        },
      });
    }

    const range = headerValue(init, "Range");
    if (!range) {
      return new Response(data.slice().buffer, {
        status: 200,
        headers: {
          "Content-Length": String(data.byteLength),
          "Accept-Ranges": "bytes",
          ETag: opts.etag,
        },
      });
    }

    const match = /^bytes=(\d+)-(\d+)$/.exec(range);
    if (!match) {
      return new Response(null, { status: 416, headers: { "Content-Range": `bytes */${data.byteLength}` } });
    }
    const start = Number(match[1]);
    const endInclusive = Number(match[2]);
    const body = data.slice(start, endInclusive + 1);
    const len = endInclusive - start + 1;
    if (len === 1) stats.probeRangeCalls += 1;
    else stats.chunkRangeCalls += 1;

    return new Response(body.buffer, {
      status: 206,
      headers: {
        "Accept-Ranges": "bytes",
        "Cache-Control": "no-transform",
        "Content-Range": `bytes ${start}-${endInclusive}/${data.byteLength}`,
        "Content-Length": String(body.byteLength),
        ETag: opts.etag,
      },
    });
  }) as typeof fetch;

  return {
    stats,
    restore: () => {
      globalThis.fetch = original;
    },
  };
}

type ConcurrencyFetchStats = {
  chunkRangeCalls: number;
  maxChunkInflight: number;
};

function installMockRangeFetchWithConcurrency(
  data: Uint8Array,
  opts: { etag: string },
): {
  stats: ConcurrencyFetchStats;
  control: { waitForChunkInflightAtLeast: (n: number) => Promise<void>; releaseAll: () => void };
  restore: () => void;
} {
  const original = globalThis.fetch;
  const stats: ConcurrencyFetchStats = { chunkRangeCalls: 0, maxChunkInflight: 0 };

  function headerValue(init: RequestInit | undefined, name: string): string | null {
    const h = init?.headers;
    if (!h) return null;
    if (h instanceof Headers) return h.get(name);
    if (Array.isArray(h)) {
      for (const [k, v] of h) {
        if (k.toLowerCase() === name.toLowerCase()) return v;
      }
      return null;
    }
    const rec = h as Record<string, string>;
    for (const [k, v] of Object.entries(rec)) {
      if (k.toLowerCase() === name.toLowerCase()) return v;
    }
    return null;
  }

  let chunkInflight = 0;
  let released = false;
  const releaseWaiters: Array<() => void> = [];

  type InflightWaiter = { target: number; resolve: () => void };
  const inflightWaiters: InflightWaiter[] = [];
  const notifyInflight = () => {
    for (let i = inflightWaiters.length - 1; i >= 0; i -= 1) {
      const waiter = inflightWaiters[i]!;
      if (stats.maxChunkInflight >= waiter.target) {
        inflightWaiters.splice(i, 1);
        waiter.resolve();
      }
    }
  };

  const releaseAll = () => {
    released = true;
    while (releaseWaiters.length > 0) {
      releaseWaiters.shift()!();
    }
  };

  const waitForChunkInflightAtLeast = async (n: number): Promise<void> => {
    if (stats.maxChunkInflight >= n) return;
    await new Promise<void>((resolve) => {
      inflightWaiters.push({ target: n, resolve });
    });
  };

  globalThis.fetch = (async (_input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const method = (init?.method ?? "GET").toUpperCase();

    if (method === "HEAD") {
      return new Response(null, {
        status: 200,
        headers: {
          "Content-Length": String(data.byteLength),
          "Accept-Ranges": "bytes",
          ETag: opts.etag,
        },
      });
    }

    const range = headerValue(init, "Range");
    if (!range) {
      return new Response(data.slice().buffer, {
        status: 200,
        headers: {
          "Content-Length": String(data.byteLength),
          "Accept-Ranges": "bytes",
          ETag: opts.etag,
        },
      });
    }

    const match = /^bytes=(\d+)-(\d+)$/.exec(range);
    if (!match) {
      return new Response(null, { status: 416, headers: { "Content-Range": `bytes */${data.byteLength}` } });
    }

    const start = Number(match[1]);
    const endInclusive = Number(match[2]);
    const body = data.slice(start, endInclusive + 1);
    const len = endInclusive - start + 1;

    const makeRangeResp = () =>
      new Response(body.buffer, {
        status: 206,
        headers: {
          "Accept-Ranges": "bytes",
          "Cache-Control": "no-transform",
          "Content-Range": `bytes ${start}-${endInclusive}/${data.byteLength}`,
          "Content-Length": String(body.byteLength),
          ETag: opts.etag,
        },
      });

    // Don't delay the 0-0 probe.
    if (len === 1) {
      return makeRangeResp();
    }

    stats.chunkRangeCalls += 1;
    chunkInflight += 1;
    stats.maxChunkInflight = Math.max(stats.maxChunkInflight, chunkInflight);
    notifyInflight();

    const resolveResp = () => {
      chunkInflight -= 1;
      return makeRangeResp();
    };

    if (released) {
      return resolveResp();
    }

    return await new Promise<Response>((resolve) => {
      releaseWaiters.push(() => resolve(resolveResp()));
    });
  }) as typeof fetch;

  return {
    stats,
    control: { waitForChunkInflightAtLeast, releaseAll },
    restore: () => {
      releaseAll();
      globalThis.fetch = original;
    },
  };
}

let restoreOpfs: (() => void) | null = null;
let restoreFetch: (() => void) | null = null;

afterEach(async () => {
  restoreFetch?.();
  restoreFetch = null;
  restoreOpfs?.();
  restoreOpfs = null;
});

describe("RemoteStreamingDisk (OPFS chunk cache)", () => {
  it("touches OPFS cache meta.json lastAccessedAtMs on cache-hit reads", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(0);
    try {
      const root = new MemoryDirectoryHandle("root");
      restoreOpfs = installMemoryOpfs(root).restore;

      const blockSize = 512;
      const cacheLimitBytes = blockSize * 8;
      const image = makeTestImage(blockSize * 2);
      const mock = installMockRangeFetch(image, { etag: '"e1"' });
      restoreFetch = mock.restore;

      const url = "https://example.test/disk.img";
      const common = {
        blockSize,
        cacheBackend: "opfs" as const,
        cacheLimitBytes,
        prefetchSequentialBlocks: 0,
        cacheImageId: "img-1",
        cacheVersion: "v1",
      };

      const disk = await RemoteStreamingDisk.open(url, common);

      // Prime the cache with one read (block 0).
      await disk.read(0, 16);
      expect(mock.stats.chunkRangeCalls).toBe(1);

      const cacheKey = await stableCacheKey(url, common);
      const manager = await RemoteCacheManager.openOpfs();
      const meta1 = await manager.readMeta(cacheKey);
      expect(meta1).not.toBeNull();

      // Advance time beyond the disk's meta-touch throttle interval.
      vi.advanceTimersByTime(61_000);

      // Second read should be a cache hit (no extra Range fetch).
      const before = mock.stats.chunkRangeCalls;
      await disk.read(0, 16);
      expect(mock.stats.chunkRangeCalls).toBe(before);

      // Touch is fire-and-forget; wait for it to land before asserting.
      let meta2 = await manager.readMeta(cacheKey);
      for (let i = 0; i < 10 && meta2 && meta2.lastAccessedAtMs <= meta1!.lastAccessedAtMs; i++) {
        await Promise.resolve();
        meta2 = await manager.readMeta(cacheKey);
      }
      expect(meta2).not.toBeNull();
      expect(meta2!.lastAccessedAtMs).toBeGreaterThan(meta1!.lastAccessedAtMs);

      await disk.close();
    } finally {
      vi.useRealTimers();
    }
  });

  it("caches fetched blocks in OPFS and reuses them on subsequent reads", async () => {
    const root = new MemoryDirectoryHandle("root");
    restoreOpfs = installMemoryOpfs(root).restore;

    const blockSize = 512;
    const cacheLimitBytes = blockSize * 8;
    const image = makeTestImage(blockSize * 3);
    const mock = installMockRangeFetch(image, { etag: '"e1"' });
    restoreFetch = mock.restore;

    const disk = await RemoteStreamingDisk.open("https://example.test/disk.img", {
      blockSize,
      cacheBackend: "opfs",
      cacheLimitBytes,
      prefetchSequentialBlocks: 0,
      cacheImageId: "img-1",
      cacheVersion: "v1",
    });

    const before = mock.stats.chunkRangeCalls;
    const first = await disk.read(0, 16);
    expect(Array.from(first)).toEqual(Array.from(image.subarray(0, 16)));
    expect(mock.stats.chunkRangeCalls).toBe(before + 1);

    const second = await disk.read(0, 16);
    expect(Array.from(second)).toEqual(Array.from(image.subarray(0, 16)));
    expect(mock.stats.chunkRangeCalls).toBe(before + 1);

    const status = await disk.getCacheStatus();
    expect(status.cachedBytes).toBe(blockSize);
    expect(status.cachedRanges).toEqual([{ start: 0, end: blockSize }]);
    expect(status.cacheLimitBytes).toBe(cacheLimitBytes);

    await disk.close();
  });

  it("evicts least-recently-used blocks when exceeding cacheLimitBytes", async () => {
    const root = new MemoryDirectoryHandle("root");
    restoreOpfs = installMemoryOpfs(root).restore;

    const blockSize = 512;
    const cacheLimitBytes = blockSize * 2;
    const image = makeTestImage(blockSize * 3);
    const mock = installMockRangeFetch(image, { etag: '"e1"' });
    restoreFetch = mock.restore;

    const disk = await RemoteStreamingDisk.open("https://example.test/disk.img", {
      blockSize,
      cacheBackend: "opfs",
      cacheLimitBytes,
      prefetchSequentialBlocks: 0,
      cacheImageId: "img-1",
      cacheVersion: "v1",
    });

    await disk.read(0, 1); // fetch block 0
    await disk.read(blockSize, 1); // fetch block 1
    expect(mock.stats.chunkRangeCalls).toBe(2);

    // Touch block 0 so block 1 becomes LRU.
    await disk.read(0, 1);
    expect(mock.stats.chunkRangeCalls).toBe(2);

    // Fetch block 2: should evict block 1.
    await disk.read(blockSize * 2, 1);
    expect(mock.stats.chunkRangeCalls).toBe(3);

    const status = await disk.getCacheStatus();
    expect(status.cachedBytes).toBe(cacheLimitBytes);
    expect(status.cachedRanges).toEqual([
      { start: 0, end: blockSize },
      { start: blockSize * 2, end: blockSize * 3 },
    ]);

    // Block 0 should still be cached (no extra fetch).
    await disk.read(0, 1);
    expect(mock.stats.chunkRangeCalls).toBe(3);

    // Block 1 should have been evicted (re-fetch).
    await disk.read(blockSize, 1);
    expect(mock.stats.chunkRangeCalls).toBe(4);

    await disk.close();
  });

  it("heals cache metadata when a chunk file is missing", async () => {
    const root = new MemoryDirectoryHandle("root");
    restoreOpfs = installMemoryOpfs(root).restore;

    const blockSize = 512;
    const cacheLimitBytes = blockSize * 8;
    const image = makeTestImage(blockSize * 2);
    const mock = installMockRangeFetch(image, { etag: '"e1"' });
    restoreFetch = mock.restore;

    const common = {
      blockSize,
      cacheBackend: "opfs" as const,
      cacheLimitBytes,
      prefetchSequentialBlocks: 0,
      cacheImageId: "img-1",
      cacheVersion: "v1",
    };

    const disk1 = await RemoteStreamingDisk.open("https://example.test/disk.img", common);
    await disk1.read(0, 1);
    expect(mock.stats.chunkRangeCalls).toBe(1);
    await disk1.close();

    const cacheKey = await RemoteCacheManager.deriveCacheKey({
      imageId: common.cacheImageId,
      version: common.cacheVersion,
      deliveryType: remoteRangeDeliveryType(blockSize),
    });
    const chunksDir = await getDir(root, ["aero", "disks", "remote-cache", cacheKey, "chunks"]);
    await chunksDir.removeEntry("0.bin");

    const disk2 = await RemoteStreamingDisk.open("https://example.test/disk.img", common);
    await disk2.read(0, 1);
    // Missing chunk file should force a re-fetch.
    expect(mock.stats.chunkRangeCalls).toBe(2);
    await disk2.close();
  });

  it("disables OPFS caching after a QuotaExceededError write (non-fatal)", async () => {
    const root = new MemoryDirectoryHandle("root");
    restoreOpfs = installMemoryOpfs(root).restore;

    const blockSize = 512;
    const cacheLimitBytes = blockSize * 8;
    const image = makeTestImage(blockSize * 2);
    const mock = installMockRangeFetch(image, { etag: '"e1"' });
    restoreFetch = mock.restore;

    const originalCreateWritable = MemoryFileHandle.prototype.createWritable;
    let chunkWritableCalls = 0;
    (MemoryFileHandle.prototype as unknown as { createWritable: unknown }).createWritable = async function (
      this: MemoryFileHandle,
      options?: { keepExistingData?: boolean },
    ) {
      const inner = await originalCreateWritable.call(this, options);
      const fileName = this.name;
      if (!fileName.endsWith(".bin")) return inner;
      chunkWritableCalls += 1;
      return {
        write: async () => {
          throw new DOMException("Quota exceeded", "QuotaExceededError");
        },
        close: async () => undefined,
        abort: async () => undefined,
      };
    };

    try {
      const disk = await RemoteStreamingDisk.open("https://example.test/disk.img", {
        blockSize,
        cacheBackend: "opfs",
        cacheLimitBytes,
        prefetchSequentialBlocks: 0,
        cacheImageId: "img-1",
        cacheVersion: "v1",
      });

      const before = mock.stats.chunkRangeCalls;
      const first = await disk.read(0, 16);
      expect(Array.from(first)).toEqual(Array.from(image.subarray(0, 16)));
      expect(mock.stats.chunkRangeCalls).toBe(before + 1);

      const chunkWritesAfterFirstRead = chunkWritableCalls;
      expect(chunkWritesAfterFirstRead).toBeGreaterThan(0);

      const telemetry = disk.getTelemetrySnapshot();
      expect(telemetry.cacheLimitBytes).toBe(0);

      const second = await disk.read(0, 16);
      expect(Array.from(second)).toEqual(Array.from(image.subarray(0, 16)));
      // Cache is disabled after the quota failure, so we should re-fetch from the network.
      expect(mock.stats.chunkRangeCalls).toBe(before + 2);
      // And we should not attempt any further OPFS cache writes.
      expect(chunkWritableCalls).toBe(chunkWritesAfterFirstRead);

      await disk.close();
    } finally {
      (MemoryFileHandle.prototype as unknown as { createWritable: unknown }).createWritable = originalCreateWritable;
    }
  });

  it("disables OPFS caching after a QuotaExceededError during flush (non-fatal)", async () => {
    const root = new MemoryDirectoryHandle("root");
    restoreOpfs = installMemoryOpfs(root).restore;

    const blockSize = 512;
    const cacheLimitBytes = blockSize * 8;
    const image = makeTestImage(blockSize * 2);
    const mock = installMockRangeFetch(image, { etag: '"e1"' });
    restoreFetch = mock.restore;

    const disk = await RemoteStreamingDisk.open("https://example.test/disk.img", {
      blockSize,
      cacheBackend: "opfs",
      cacheLimitBytes,
      prefetchSequentialBlocks: 0,
      cacheImageId: "img-1",
      cacheVersion: "v1",
    });

    const before = mock.stats.chunkRangeCalls;
    await disk.read(0, 16);
    expect(mock.stats.chunkRangeCalls).toBe(before + 1);

    // Cache hit: should touch the LRU index (index.json) without extra network fetches.
    await disk.read(0, 16);
    expect(mock.stats.chunkRangeCalls).toBe(before + 1);

    const originalCreateWritable = MemoryFileHandle.prototype.createWritable;
    let indexWritableCalls = 0;
    (MemoryFileHandle.prototype as unknown as { createWritable: unknown }).createWritable = async function (
      this: MemoryFileHandle,
      options?: { keepExistingData?: boolean },
    ) {
      if (this.name !== "index.json") {
        return await originalCreateWritable.call(this, options);
      }
      indexWritableCalls += 1;
      return {
        write: async () => {
          throw new DOMException("Quota exceeded", "QuotaExceededError");
        },
        close: async () => undefined,
        abort: async () => undefined,
      };
    };

    try {
      await expect(disk.flush()).resolves.toBeUndefined();
      expect(indexWritableCalls).toBeGreaterThan(0);

      const telemetry = disk.getTelemetrySnapshot();
      expect(telemetry.cacheLimitBytes).toBe(0);

      // Cache is disabled after the quota failure, so we should re-fetch from the network.
      await disk.read(0, 16);
      expect(mock.stats.chunkRangeCalls).toBe(before + 2);

      await disk.close();
    } finally {
      (MemoryFileHandle.prototype as unknown as { createWritable: unknown }).createWritable = originalCreateWritable;
    }
  });

  it("does not touch OPFS/IDB when cacheLimitBytes is 0", async () => {
    const blockSize = 512;
    const image = makeTestImage(blockSize * 2);
    const mock = installMockRangeFetch(image, { etag: '"e1"' });
    restoreFetch = mock.restore;

    const disk = await RemoteStreamingDisk.open("https://example.test/disk.img", {
      blockSize,
      cacheLimitBytes: 0,
      prefetchSequentialBlocks: 0,
    });

    await disk.read(0, 1);
    await disk.read(0, 1);
    expect(mock.stats.chunkRangeCalls).toBe(2);

    await disk.close();
  });

  it("issues concurrent Range requests for multi-block reads", async () => {
    const blockSize = 512;
    const image = makeTestImage(blockSize * 4);
    const mock = installMockRangeFetchWithConcurrency(image, { etag: '"e1"' });
    restoreFetch = mock.restore;

    const disk = await RemoteStreamingDisk.open("https://example.test/disk.img", {
      blockSize,
      cacheLimitBytes: 0,
      prefetchSequentialBlocks: 0,
    });

    const readPromise = disk.read(0, blockSize * 3);
    let sawConcurrency = false;
    let bytes: Uint8Array | null = null;
    try {
      await Promise.race([
        mock.control.waitForChunkInflightAtLeast(2).then(() => {
          sawConcurrency = true;
        }),
        new Promise<void>((_, reject) =>
          setTimeout(() => reject(new Error("expected concurrent Range requests (chunk inflight never exceeded 1)")), 1000),
        ),
      ]);
      expect(sawConcurrency).toBe(true);
      expect(mock.stats.maxChunkInflight).toBeGreaterThan(1);
    } finally {
      // Ensure the read can complete even if the concurrency assertion fails.
      mock.control.releaseAll();
      bytes = await readPromise.catch(() => null);
      await disk.close().catch(() => {
        // best-effort
      });
    }

    if (!bytes) throw new Error("expected read to complete");
    expect(Array.from(bytes)).toEqual(Array.from(image.subarray(0, blockSize * 3)));
  });
});
