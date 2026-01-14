import "../../test/fake_indexeddb_auto.ts";

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { clearIdb } from "../storage/metadata";
import { IdbRemoteChunkCache, IdbRemoteChunkCacheQuotaError } from "../storage/idb_remote_chunk_cache";
import { RemoteCacheManager } from "../storage/remote_cache_manager";
import { RemoteStreamingDisk } from "./remote_disk";

function makeTestImage(size: number): Uint8Array {
  const buf = new Uint8Array(size);
  for (let i = 0; i < size; i += 1) buf[i] = (i * 13) & 0xff;
  return buf;
}

type FetchStats = {
  totalCalls: number;
  probeRangeCalls: number;
  chunkRangeCalls: number;
  seenChunkIfRanges: Array<string | null>;
};

function installMockRangeFetch(
  data: Uint8Array,
  opts: { etag: string; lastModified?: string },
): { stats: FetchStats; restore: () => void } {
  const original = globalThis.fetch;
  const stats: FetchStats = { totalCalls: 0, probeRangeCalls: 0, chunkRangeCalls: 0, seenChunkIfRanges: [] };

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

  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    stats.totalCalls += 1;
    const method = (init?.method ?? "GET").toUpperCase();

    if (method === "HEAD") {
      return new Response(null, {
        status: 200,
        headers: {
          "Content-Length": String(data.byteLength),
          "Accept-Ranges": "bytes",
          ETag: opts.etag,
          ...(opts.lastModified ? { "Last-Modified": opts.lastModified } : {}),
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
          ...(opts.lastModified ? { "Last-Modified": opts.lastModified } : {}),
        },
      });
    }

    const ifRange = headerValue(init, "If-Range");
    const match = /^bytes=(\d+)-(\d+)$/.exec(range);
    const suffix = /^bytes=-(\d+)$/.exec(range);
    if (!match && !suffix) {
      return new Response(null, { status: 416, headers: { "Content-Range": `bytes */${data.byteLength}` } });
    }
    const start = match ? Number(match[1]) : Math.max(0, data.byteLength - Number(suffix![1]));
    const endInclusive = match ? Number(match[2]) : data.byteLength - 1;
    const body = data.slice(start, endInclusive + 1);
    const len = endInclusive - start + 1;
    // `RemoteStreamingDisk` performs a 0-0 probe, plus small header/footer sniffing reads to
    // detect container formats. For these tests, we count only full block fetches as
    // `chunkRangeCalls` so assertions remain meaningful.
    const isSniff =
      // Head sniff (0-63) or truncated head when size < 64.
      (start === 0 && endInclusive === Math.min(63, data.byteLength - 1) && len <= 64) ||
      // Tail sniff is requested using a suffix range (bytes=-512), which does not collide with
      // normal block reads (bytes=start-end).
      suffix !== null;
    if (len === 1 || isSniff) {
      stats.probeRangeCalls += 1;
    } else {
      stats.chunkRangeCalls += 1;
      stats.seenChunkIfRanges.push(ifRange);
    }

    return new Response(body.buffer, {
      status: 206,
      headers: {
        "Accept-Ranges": "bytes",
        "Cache-Control": "no-transform",
        "Content-Range": `bytes ${start}-${endInclusive}/${data.byteLength}`,
        "Content-Length": String(body.byteLength),
        ETag: opts.etag,
        ...(opts.lastModified ? { "Last-Modified": opts.lastModified } : {}),
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

function installQuotaExceededOnRemoteChunksPut(disk: RemoteStreamingDisk): { putCalls: { count: number }; restore: () => void } {
  const idbCache = (disk as unknown as { idbCache?: unknown }).idbCache as
    | { db?: { transaction: (...args: any[]) => any } }
    | undefined;
  if (!idbCache?.db) throw new Error("expected disk to have an IDB cache");

  const db = idbCache.db;
  const putCalls = { count: 0 };
  const originalTransaction = db.transaction.bind(db);

  db.transaction = (storeNames: any, mode: any) => {
    const tx = originalTransaction(storeNames, mode);
    const originalObjectStore = tx.objectStore.bind(tx);
    tx.objectStore = (name: any) => {
      const store = originalObjectStore(name);
      if (mode === "readwrite" && name === "remote_chunks") {
        store.put = (_value: any, _key?: any) => {
          putCalls.count += 1;

          // Model a request that fails asynchronously and aborts the transaction,
          // similar to how IndexedDB signals QuotaExceededError.
          const req: { error: unknown; onerror: null | (() => void) } = { error: null, onerror: null };
          const txHooks = tx as unknown as { requestStarted?: () => void; requestFinished?: () => void; error?: unknown };
          txHooks.requestStarted?.();
          queueMicrotask(() => {
            const err = new DOMException("quota exceeded", "QuotaExceededError");
            req.error = err;
            txHooks.error = err;
            try {
              req.onerror?.();
            } finally {
              tx.onerror?.();
              tx.onabort?.();
              txHooks.requestFinished?.();
            }
          });
          return req;
        };
      }
      return store;
    };
    return tx;
  };

  return {
    putCalls,
    restore: () => {
      db.transaction = originalTransaction;
    },
  };
}

describe("RemoteStreamingDisk (IndexedDB cache)", () => {
  // For these tests we want caching enabled without eviction. Use a large cap (or `null`)
  // so we don't evict during the test runs.
  const cacheLimitBytes = 1024 * 1024 * 1024;

  beforeEach(async () => {
    await clearIdb();
  });

  afterEach(async () => {
    vi.restoreAllMocks();
    await clearIdb();
  });

  it("rejects block sizes larger than 64MiB", async () => {
    await expect(
      RemoteStreamingDisk.open("https://example.invalid/disk.img", {
        blockSize: 128 * 1024 * 1024,
        cacheBackend: "idb",
        cacheLimitBytes,
        prefetchSequentialBlocks: 0,
      }),
    ).rejects.toThrow(/blockSize.*max/i);
  });

  it("rejects excessive prefetchSequentialBlocks", async () => {
    await expect(
      RemoteStreamingDisk.open("https://example.invalid/disk.img", {
        blockSize: 1024 * 1024,
        cacheBackend: "idb",
        cacheLimitBytes,
        prefetchSequentialBlocks: 1025,
      }),
    ).rejects.toThrow(/prefetchSequentialBlocks.*max/i);
  });

  it("rejects excessive prefetchSequentialBlocks byte volume", async () => {
    await expect(
      RemoteStreamingDisk.open("https://example.invalid/disk.img", {
        blockSize: 64 * 1024 * 1024,
        cacheBackend: "idb",
        cacheLimitBytes,
        prefetchSequentialBlocks: 9,
      }),
    ).rejects.toThrow(/prefetch bytes too large/i);
  });

  it("rejects remote qcow2 images by content sniffing", async () => {
    const image = new Uint8Array(1024);
    image.set([0x51, 0x46, 0x49, 0xfb], 0); // "QFI\xfb"
    new DataView(image.buffer).setUint32(4, 3, false);
    const mock = installMockRangeFetch(image, { etag: '"e1"' });

    await expect(
      RemoteStreamingDisk.open("https://example.test/disk.img", {
        blockSize: 1024,
        cacheBackend: "idb",
        cacheLimitBytes: 0,
        prefetchSequentialBlocks: 0,
      }),
    ).rejects.toThrow(/qcow2/i);

    mock.restore();
  });

  it("rejects remote aerospar images by content sniffing", async () => {
    const image = new Uint8Array(1024);
    image.set([0x41, 0x45, 0x52, 0x4f, 0x53, 0x50, 0x41, 0x52], 0); // "AEROSPAR"
    new DataView(image.buffer).setUint32(8, 1, true);
    const mock = installMockRangeFetch(image, { etag: '"e1"' });

    await expect(
      RemoteStreamingDisk.open("https://example.test/disk.img", {
        blockSize: 1024,
        cacheBackend: "idb",
        cacheLimitBytes: 0,
        prefetchSequentialBlocks: 0,
      }),
    ).rejects.toThrow(/aerospar/i);

    mock.restore();
  });

  it("rejects remote VHD images by content sniffing", async () => {
    const image = new Uint8Array(1024);
    const footer = new Uint8Array(512);
    footer.set([0x63, 0x6f, 0x6e, 0x65, 0x63, 0x74, 0x69, 0x78], 0); // "conectix"
    const dv = new DataView(footer.buffer);
    dv.setUint32(12, 0x0001_0000, false);
    dv.setBigUint64(16, 0xffff_ffff_ffff_ffffn, false); // fixed disk
    dv.setBigUint64(48, 512n, false); // current size (bytes)
    dv.setUint32(60, 2, false); // disk type: fixed
    image.set(footer, 512);
    const mock = installMockRangeFetch(image, { etag: '"e1"' });

    await expect(
      RemoteStreamingDisk.open("https://example.test/disk.img", {
        blockSize: 1024,
        cacheBackend: "idb",
        cacheLimitBytes: 0,
        prefetchSequentialBlocks: 0,
      }),
    ).rejects.toThrow(/vhd/i);

    mock.restore();
  });

  it("caches fetched blocks in IndexedDB and reuses them on subsequent reads", async () => {
    const blockSize = 1024 * 1024;
    // NOTE: `RemoteStreamingDisk` treats `cacheLimitBytes=0` as "cache disabled".
    // Use a positive limit here so the IDB cache is enabled.
    const cacheLimitBytes = blockSize * 8;
    const image = makeTestImage(blockSize * 3);
    const mock = installMockRangeFetch(image, { etag: '"e1"' });

    const disk = await RemoteStreamingDisk.open("https://example.test/disk.img", {
      blockSize,
      cacheBackend: "idb",
      cacheLimitBytes,
      prefetchSequentialBlocks: 0,
    });

    const before = mock.stats.chunkRangeCalls;
    const first = await disk.read(0, 16);
    expect(Array.from(first)).toEqual(Array.from(image.subarray(0, 16)));
    expect(mock.stats.chunkRangeCalls).toBe(before + 1);

    const second = await disk.read(0, 16);
    expect(Array.from(second)).toEqual(Array.from(image.subarray(0, 16)));
    expect(mock.stats.chunkRangeCalls).toBe(before + 1);

    disk.close();
    mock.restore();
  });

  it("falls back to IndexedDB when OPFS cache init fails", async () => {
    const blockSize = 1024 * 1024;
    const cacheLimitBytes = blockSize * 8;
    const image = makeTestImage(blockSize * 2);
    const mock = installMockRangeFetch(image, { etag: '"e1"' });

    const openOpfsSpy = vi.spyOn(RemoteCacheManager, "openOpfs").mockRejectedValue(new Error("OPFS unavailable"));
    const idbOpenSpy = vi.spyOn(IdbRemoteChunkCache, "open");

    const disk = await RemoteStreamingDisk.open("https://example.test/disk.img", {
      blockSize,
      cacheBackend: "opfs",
      cacheLimitBytes,
      prefetchSequentialBlocks: 0,
    });

    expect(openOpfsSpy).toHaveBeenCalled();
    expect(idbOpenSpy).toHaveBeenCalled();

    const before = mock.stats.chunkRangeCalls;
    const first = await disk.read(0, 16);
    expect(Array.from(first)).toEqual(Array.from(image.subarray(0, 16)));
    expect(mock.stats.chunkRangeCalls).toBe(before + 1);

    const second = await disk.read(0, 16);
    expect(Array.from(second)).toEqual(Array.from(image.subarray(0, 16)));
    // If we successfully fell back to the IDB cache, the second read should be a cache hit.
    expect(mock.stats.chunkRangeCalls).toBe(before + 1);

    disk.close();
    mock.restore();
  });

  it("falls back to cache-disabled mode when OPFS cache init fails and IndexedDB is unavailable", async () => {
    const blockSize = 1024 * 1024;
    const cacheLimitBytes = blockSize * 8;
    const image = makeTestImage(blockSize * 2);
    const mock = installMockRangeFetch(image, { etag: '"e1"' });
    const globals = globalThis as unknown as { indexedDB?: unknown };
    const originalIndexedDB = globals.indexedDB;

    const openOpfsSpy = vi.spyOn(RemoteCacheManager, "openOpfs").mockRejectedValue(new Error("OPFS unavailable"));
    const idbOpenSpy = vi.spyOn(IdbRemoteChunkCache, "open");

    // Simulate environments without IndexedDB (e.g. older webviews / sandboxed contexts). The disk
    // should still open and read correctly, just without caching.
    globals.indexedDB = undefined;

    let disk: RemoteStreamingDisk | null = null;
    try {
      disk = await RemoteStreamingDisk.open("https://example.test/disk.img", {
        blockSize,
        cacheBackend: "opfs",
        cacheLimitBytes,
        prefetchSequentialBlocks: 0,
      });

      expect(openOpfsSpy).toHaveBeenCalled();
      expect(idbOpenSpy).not.toHaveBeenCalled();

      expect(disk.getTelemetrySnapshot().cacheLimitBytes).toBe(0);

      const before = mock.stats.chunkRangeCalls;
      const first = await disk.read(0, 16);
      expect(Array.from(first)).toEqual(Array.from(image.subarray(0, 16)));
      expect(mock.stats.chunkRangeCalls).toBe(before + 1);

      const beforeSecond = mock.stats.chunkRangeCalls;
      const second = await disk.read(0, 16);
      expect(Array.from(second)).toEqual(Array.from(image.subarray(0, 16)));
      // Cache disabled -> must refetch.
      expect(mock.stats.chunkRangeCalls).toBe(beforeSecond + 1);
    } finally {
      disk?.close();
      mock.restore();
      globals.indexedDB = originalIndexedDB;
    }
  });

  it("tolerates IndexedDB quota errors when persisting cached blocks", async () => {
    const blockSize = 1024 * 1024;
    const cacheLimitBytes = blockSize * 8;
    const image = makeTestImage(blockSize * 2);
    const mock = installMockRangeFetch(image, { etag: '"e1"' });

    const disk = await RemoteStreamingDisk.open("https://example.test/disk.img", {
      blockSize,
      cacheBackend: "idb",
      cacheLimitBytes,
      prefetchSequentialBlocks: 0,
    });

    const quota = installQuotaExceededOnRemoteChunksPut(disk);

    const first = await disk.read(0, 16);
    expect(Array.from(first)).toEqual(Array.from(image.subarray(0, 16)));

    // Cache put should be attempted twice (initial + retry) but must not fail the read.
    expect(quota.putCalls.count).toBe(2);
    expect(disk.getTelemetrySnapshot().cacheLimitBytes).toBe(0);

    const before = mock.stats.chunkRangeCalls;
    const second = await disk.read(0, 16);
    expect(Array.from(second)).toEqual(Array.from(image.subarray(0, 16)));

    // With caching disabled, the second read must re-fetch.
    expect(mock.stats.chunkRangeCalls).toBe(before + 1);
    expect(quota.putCalls.count).toBe(2);

    quota.restore();
    disk.close();
    mock.restore();
  });

  it("tolerates IndexedDB quota errors when updating access metadata for cached blocks", async () => {
    const blockSize = 1024 * 1024;
    const cacheLimitBytes = blockSize * 8;
    const image = makeTestImage(blockSize * 2);
    const mock = installMockRangeFetch(image, { etag: '"e1"' });

    const disk = await RemoteStreamingDisk.open("https://example.test/disk.img", {
      blockSize,
      cacheBackend: "idb",
      cacheLimitBytes,
      prefetchSequentialBlocks: 0,
    });

    // Prime the persistent cache with block 0.
    await disk.read(0, 16);
    expect(mock.stats.chunkRangeCalls).toBe(1);

    // Force subsequent reads to consult IndexedDB by disabling the in-memory LRU.
    const idbCache = (disk as unknown as { idbCache?: any }).idbCache;
    if (!idbCache) throw new Error("expected idb cache");
    idbCache.maxCachedChunks = 0;
    idbCache.cache?.clear?.();

    // Simulate quota errors on the access-metadata update path (`remote_chunks.put()`).
    const quota = installQuotaExceededOnRemoteChunksPut(disk);

    const before = mock.stats.chunkRangeCalls;
    const bytes = await disk.read(0, 16);
    expect(Array.from(bytes)).toEqual(Array.from(image.subarray(0, 16)));
    // Still a cache hit: should not re-fetch from the network.
    expect(mock.stats.chunkRangeCalls).toBe(before);
    expect(quota.putCalls.count).toBe(1);

    quota.restore();
    disk.close();
    mock.restore();
  });

  it("treats cache open quota failures as non-fatal (cache disabled)", async () => {
    const blockSize = 1024 * 1024;
    const cacheLimitBytes = blockSize * 8;
    const image = makeTestImage(blockSize * 2);
    const mock = installMockRangeFetch(image, { etag: '"e1"' });

    const originalOpen = IdbRemoteChunkCache.open;
    IdbRemoteChunkCache.open = (async () => {
      throw new IdbRemoteChunkCacheQuotaError();
    }) as unknown as typeof IdbRemoteChunkCache.open;

    try {
      const disk = await RemoteStreamingDisk.open("https://example.test/disk.img", {
        blockSize,
        cacheBackend: "idb",
        cacheLimitBytes,
        prefetchSequentialBlocks: 0,
      });

      expect(disk.getTelemetrySnapshot().cacheLimitBytes).toBe(0);

      const before = mock.stats.chunkRangeCalls;
      await disk.read(0, 16);
      await disk.read(0, 16);

      // With caching disabled, both reads must hit the network.
      expect(mock.stats.chunkRangeCalls).toBe(before + 2);

      disk.close();
    } finally {
      IdbRemoteChunkCache.open = originalOpen;
      mock.restore();
    }
  });

  it("closes the IDB cache if it fails after open (e.g. getStatus quota failure)", async () => {
    const blockSize = 1024 * 1024;
    const cacheLimitBytes = blockSize * 8;
    const image = makeTestImage(blockSize * 2);
    const mock = installMockRangeFetch(image, { etag: '"e1"' });

    const originalOpen = IdbRemoteChunkCache.open;
    let closed = false;
    IdbRemoteChunkCache.open = (async () => {
      return {
        getStatus: async () => {
          throw new IdbRemoteChunkCacheQuotaError();
        },
        close: () => {
          closed = true;
        },
      } as unknown as IdbRemoteChunkCache;
    }) as unknown as typeof IdbRemoteChunkCache.open;

    try {
      const disk = await RemoteStreamingDisk.open("https://example.test/disk.img", {
        blockSize,
        cacheBackend: "idb",
        cacheLimitBytes,
        prefetchSequentialBlocks: 0,
      });

      expect(closed).toBe(true);
      expect(disk.getTelemetrySnapshot().cacheLimitBytes).toBe(0);

      const before = mock.stats.chunkRangeCalls;
      await disk.read(0, 16);
      // With cache disabled, this must hit the network.
      expect(mock.stats.chunkRangeCalls).toBe(before + 1);

      disk.close();
    } finally {
      IdbRemoteChunkCache.open = originalOpen;
      mock.restore();
    }
  });

  it("treats clearCache quota failures as non-fatal (cache disabled)", async () => {
    const blockSize = 1024 * 1024;
    const cacheLimitBytes = blockSize * 8;
    const image = makeTestImage(blockSize * 2);
    const mock = installMockRangeFetch(image, { etag: '"e1"' });

    const disk = await RemoteStreamingDisk.open("https://example.test/disk.img", {
      blockSize,
      cacheBackend: "idb",
      cacheLimitBytes,
      prefetchSequentialBlocks: 0,
    });

    const cache = (disk as unknown as { idbCache?: { clear: () => Promise<void> } }).idbCache;
    if (!cache) throw new Error("expected idb cache");

    cache.clear = async () => {
      throw new IdbRemoteChunkCacheQuotaError();
    };

    await expect(disk.clearCache()).resolves.toBeUndefined();
    expect(disk.getTelemetrySnapshot().cacheLimitBytes).toBe(0);

    disk.close();
    mock.restore();
  });

  it("does not wipe telemetry for reads that occur while clearCache is in-flight", async () => {
    const blockSize = 1024 * 1024;
    const cacheLimitBytes = blockSize * 8;
    const image = makeTestImage(blockSize * 2);
    const mock = installMockRangeFetch(image, { etag: '"e1"' });

    const disk = await RemoteStreamingDisk.open("https://example.test/disk.img", {
      blockSize,
      cacheBackend: "idb",
      cacheLimitBytes,
      prefetchSequentialBlocks: 0,
    });

    const cache = (disk as unknown as { idbCache?: { clear: () => Promise<void> } }).idbCache;
    if (!cache) throw new Error("expected idb cache");

    const originalClear = cache.clear.bind(cache);
    // Don't model this as `(() => void) | null`: TS doesn't understand that the
    // Promise executor runs synchronously, so it narrows the variable to `null`
    // at callsites in the outer scope. Start with a no-op and replace it in the
    // executor instead.
    let releaseClear = () => {};
    const releasePromise = new Promise<void>((resolve) => {
      releaseClear = () => resolve();
    });

    let started = () => {};
    const startedPromise = new Promise<void>((resolve) => {
      started = () => resolve();
    });

    cache.clear = async () => {
      started();
      await releasePromise;
      await originalClear();
    };

    const clearPromise = disk.clearCache();
    await startedPromise;

    const bytes = await disk.read(0, 16);
    expect(Array.from(bytes)).toEqual(Array.from(image.subarray(0, 16)));

    releaseClear();
    await clearPromise;

    const t = disk.getTelemetrySnapshot();
    expect(t.requests).toBe(1);
    expect(t.bytesDownloaded).toBe(blockSize);

    disk.close();
    mock.restore();
  });

  it("invalidates the IDB cache when the remote ETag changes", async () => {
    const blockSize = 1024 * 1024;
    const cacheLimitBytes = blockSize * 8;
    const image = makeTestImage(blockSize * 2);

    const mock1 = installMockRangeFetch(image, { etag: '"e1"' });
    const disk1 = await RemoteStreamingDisk.open("https://example.test/disk.img", {
      blockSize,
      cacheBackend: "idb",
      cacheLimitBytes,
      prefetchSequentialBlocks: 0,
    });
    await disk1.read(0, 16);
    expect(mock1.stats.chunkRangeCalls).toBe(1);
    disk1.close();
    mock1.restore();

    const mock2 = installMockRangeFetch(image, { etag: '"e2"' });
    const disk2 = await RemoteStreamingDisk.open("https://example.test/disk.img", {
      blockSize,
      cacheBackend: "idb",
      cacheLimitBytes,
      prefetchSequentialBlocks: 0,
    });
    await disk2.read(0, 16);
    expect(mock2.stats.chunkRangeCalls).toBe(1);
    disk2.close();
    mock2.restore();
  });

  it("reuses the IDB cache across refreshed URLs when cache identity is stable", async () => {
    const blockSize = 1024 * 1024;
    const cacheLimitBytes = blockSize * 8;
    const image = makeTestImage(blockSize * 2);
    const mock = installMockRangeFetch(image, { etag: '"e1"' });

    const opts = {
      blockSize,
      cacheBackend: "idb" as const,
      cacheLimitBytes,
      prefetchSequentialBlocks: 0,
      cacheImageId: "img-1",
      cacheVersion: "v1",
    };

    const disk1 = await RemoteStreamingDisk.open("https://example.test/disk.img?token=a", opts);
    await disk1.read(0, 16);
    expect(mock.stats.chunkRangeCalls).toBe(1);
    disk1.close();

    const disk2 = await RemoteStreamingDisk.open("https://example.test/disk.img?token=b", opts);
    await disk2.read(0, 16);
    expect(mock.stats.chunkRangeCalls).toBe(1);
    disk2.close();

    mock.restore();
  });

  it("invalidates the IDB cache when cacheVersion changes", async () => {
    const blockSize = 1024 * 1024;
    const cacheLimitBytes = blockSize * 8;
    const image = makeTestImage(blockSize * 2);
    const mock = installMockRangeFetch(image, { etag: '"e1"' });

    const common = {
      blockSize,
      cacheBackend: "idb" as const,
      cacheLimitBytes,
      prefetchSequentialBlocks: 0,
      cacheImageId: "img-1",
    };

    const disk1 = await RemoteStreamingDisk.open("https://example.test/disk.img?token=a", { ...common, cacheVersion: "v1" });
    await disk1.read(0, 16);
    expect(mock.stats.chunkRangeCalls).toBe(1);
    disk1.close();

    const disk2 = await RemoteStreamingDisk.open("https://example.test/disk.img?token=b", { ...common, cacheVersion: "v2" });
    await disk2.read(0, 16);
    expect(mock.stats.chunkRangeCalls).toBe(2);
    disk2.close();

    mock.restore();
  });

  it("includes If-Range for strong ETags on Range block fetches", async () => {
    const blockSize = 1024 * 1024;
    const cacheLimitBytes = blockSize * 8;
    const image = makeTestImage(blockSize * 2);
    const mock = installMockRangeFetch(image, { etag: '"e1"' });

    const disk = await RemoteStreamingDisk.open("https://example.test/disk.img", {
      blockSize,
      cacheBackend: "idb",
      cacheLimitBytes,
      prefetchSequentialBlocks: 0,
    });
    await disk.read(0, 16);

    expect(mock.stats.seenChunkIfRanges).toContain('"e1"');

    disk.close();
    mock.restore();
  });

  it("omits If-Range for weak ETags (some servers reject them)", async () => {
    const blockSize = 1024 * 1024;
    const cacheLimitBytes = blockSize * 8;
    const image = makeTestImage(blockSize * 2);
    const mock = installMockRangeFetch(image, { etag: 'W/"e1"' });

    const disk = await RemoteStreamingDisk.open("https://example.test/disk.img", {
      blockSize,
      cacheBackend: "idb",
      cacheLimitBytes,
      prefetchSequentialBlocks: 0,
    });
    await disk.read(0, 16);

    expect(mock.stats.seenChunkIfRanges).toContain(null);

    disk.close();
    mock.restore();
  });

  it("uses Last-Modified for If-Range when ETag is weak", async () => {
    const blockSize = 1024 * 1024;
    const cacheLimitBytes = blockSize * 8;
    const image = makeTestImage(blockSize * 2);
    const lastModified = "Mon, 01 Jan 2024 00:00:00 GMT";
    const mock = installMockRangeFetch(image, { etag: 'W/"e1"', lastModified });

    const disk = await RemoteStreamingDisk.open("https://example.test/disk.img", {
      blockSize,
      cacheBackend: "idb",
      cacheLimitBytes,
      prefetchSequentialBlocks: 0,
    });
    await disk.read(0, 16);

    expect(mock.stats.seenChunkIfRanges).toContain(lastModified);

    disk.close();
    mock.restore();
  });

  it("detects validator drift on 206 responses and retries successfully", async () => {
    const original = globalThis.fetch;
    const blockSize = 1024 * 1024;
    const cacheLimitBytes = blockSize * 8;
    let image = makeTestImage(blockSize * 2);
    let etag = '"e1"';
    const seenChunkIfRanges: Array<string | null> = [];
    let chunkCalls = 0;

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
      const method = (init?.method ?? "GET").toUpperCase();
      if (method === "HEAD") {
        return new Response(null, {
          status: 200,
          headers: {
            "Content-Length": String(image.byteLength),
            "Accept-Ranges": "bytes",
            ETag: etag,
          },
        });
      }

      const range = headerValue(init, "Range");
      if (!range) {
        return new Response(image.slice().buffer, {
          status: 200,
          headers: {
            "Content-Length": String(image.byteLength),
            "Accept-Ranges": "bytes",
            ETag: etag,
          },
        });
      }

      const match = /^bytes=(\d+)-(\d+)$/.exec(range);
      const suffix = /^bytes=-(\d+)$/.exec(range);
      if (!match && !suffix) {
        return new Response(null, { status: 416, headers: { "Content-Range": `bytes */${image.byteLength}` } });
      }
      const start = match ? Number(match[1]) : Math.max(0, image.byteLength - Number(suffix![1]));
      const endInclusive = match ? Number(match[2]) : image.byteLength - 1;
      const body = image.slice(start, endInclusive + 1);
      const len = endInclusive - start + 1;

      // Only record the block-aligned chunk fetches (ignore the 0-0 probe and header/footer sniffing).
      const ifRange = headerValue(init, "If-Range");
      const isSniff = (range === "bytes=-512") || (start === 0 && endInclusive <= 63 && len <= 64);
      if (len !== 1 && !isSniff) {
        chunkCalls += 1;
        seenChunkIfRanges.push(ifRange);
      }

      return new Response(body.buffer, {
        status: 206,
        headers: {
          "Accept-Ranges": "bytes",
          "Cache-Control": "no-transform",
          "Content-Range": `bytes ${start}-${endInclusive}/${image.byteLength}`,
          "Content-Length": String(body.byteLength),
          ETag: etag,
        },
      });
    }) as typeof fetch;

    try {
      const disk = await RemoteStreamingDisk.open("https://example.test/disk.img", {
        blockSize,
        cacheBackend: "idb",
        cacheLimitBytes,
        prefetchSequentialBlocks: 0,
      });

      // Cache chunk 0 under ETag e1.
      await disk.read(0, 16);

      // Mutate the server: new ETag and new content.
      image = new Uint8Array(image.length);
      image.fill(7);
      etag = '"e2"';

      // Read chunk 1: first attempt returns 206 with e2 (server ignores If-Range),
      // client detects drift, invalidates, re-probes, and retries with If-Range=e2.
      const chunk1 = await disk.read(blockSize, 16);
      expect(Array.from(chunk1)).toEqual(Array.from(image.subarray(blockSize, blockSize + 16)));

      expect(seenChunkIfRanges).toContain('"e1"');
      expect(seenChunkIfRanges).toContain('"e2"');
      expect(chunkCalls).toBeGreaterThanOrEqual(3);

      disk.close();
    } finally {
      globalThis.fetch = original;
    }
  });

  it("treats 416 responses as validator mismatches and retries successfully", async () => {
    const original = globalThis.fetch;
    const blockSize = 1024 * 1024;
    const cacheLimitBytes = blockSize * 8;
    const image = makeTestImage(blockSize * 2);
    let etag = '"e1"';
    const seenChunkIfRanges: Array<string | null> = [];
    let mismatch416Calls = 0;

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
      const method = (init?.method ?? "GET").toUpperCase();
      if (method === "HEAD") {
        return new Response(null, {
          status: 200,
          headers: {
            "Content-Length": String(image.byteLength),
            "Accept-Ranges": "bytes",
            ETag: etag,
          },
        });
      }

      const range = headerValue(init, "Range");
      if (!range) {
        return new Response(image.slice().buffer, {
          status: 200,
          headers: {
            "Content-Length": String(image.byteLength),
            "Accept-Ranges": "bytes",
            ETag: etag,
          },
        });
      }

      const match = /^bytes=(\d+)-(\d+)$/.exec(range);
      const suffix = /^bytes=-(\d+)$/.exec(range);
      if (!match && !suffix) {
        return new Response(null, { status: 416, headers: { "Content-Range": `bytes */${image.byteLength}` } });
      }
      const start = match ? Number(match[1]) : Math.max(0, image.byteLength - Number(suffix![1]));
      const endInclusive = match ? Number(match[2]) : image.byteLength - 1;
      const body = image.slice(start, endInclusive + 1);

      const ifRange = headerValue(init, "If-Range");
      const isSniff = (range === "bytes=-512") || (start === 0 && endInclusive <= 63 && body.byteLength <= 64);
      if (body.byteLength !== 1 && !isSniff) {
        seenChunkIfRanges.push(ifRange);
      }

      // Model servers that respond 416 when an If-Range validator does not match the
      // current representation.
      if (ifRange && ifRange !== etag) {
        mismatch416Calls += 1;
        return new Response(null, { status: 416, headers: { "Content-Range": `bytes */${image.byteLength}` } });
      }

      return new Response(body.buffer, {
        status: 206,
        headers: {
          "Accept-Ranges": "bytes",
          "Cache-Control": "no-transform",
          "Content-Range": `bytes ${start}-${endInclusive}/${image.byteLength}`,
          "Content-Length": String(body.byteLength),
          ETag: etag,
        },
      });
    }) as typeof fetch;

    try {
      const disk = await RemoteStreamingDisk.open("https://example.test/disk.img", {
        blockSize,
        cacheBackend: "idb",
        cacheLimitBytes,
        prefetchSequentialBlocks: 0,
      });

      // Cache chunk 0 under ETag e1.
      await disk.read(0, 16);

      // Mutate the server: new ETag but the same content/size.
      etag = '"e2"';

      // Read chunk 1: first attempt returns 416 due to If-Range=e1 mismatch,
      // client treats 416 as a validator mismatch, re-probes, and retries with If-Range=e2.
      const chunk1 = await disk.read(blockSize, 16);
      expect(Array.from(chunk1)).toEqual(Array.from(image.subarray(blockSize, blockSize + 16)));

      expect(mismatch416Calls).toBeGreaterThanOrEqual(1);
      expect(seenChunkIfRanges).toContain('"e1"');
      expect(seenChunkIfRanges).toContain('"e2"');

      disk.close();
    } finally {
      globalThis.fetch = original;
    }
  });
});
