import "../../test/fake_indexeddb_auto.ts";

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { clearIdb } from "../storage/metadata";
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
    if (!match) {
      return new Response(null, { status: 416, headers: { "Content-Range": `bytes */${data.byteLength}` } });
    }
    const start = Number(match[1]);
    const endInclusive = Number(match[2]);
    const body = data.slice(start, endInclusive + 1);
    const len = endInclusive - start + 1;
    if (len === 1) stats.probeRangeCalls += 1;
    else {
      stats.chunkRangeCalls += 1;
      stats.seenChunkIfRanges.push(ifRange);
    }

    return new Response(body.buffer, {
      status: 206,
      headers: {
        "Accept-Ranges": "bytes",
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

describe("RemoteStreamingDisk (IndexedDB cache)", () => {
  // For these tests we want caching enabled without eviction. Use a large cap (or `null`)
  // so we don't evict during the test runs.
  const cacheLimitBytes = 1024 * 1024 * 1024;

  beforeEach(async () => {
    await clearIdb();
  });

  afterEach(async () => {
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
      if (!match) {
        return new Response(null, { status: 416, headers: { "Content-Range": `bytes */${image.byteLength}` } });
      }
      const start = Number(match[1]);
      const endInclusive = Number(match[2]);
      const body = image.slice(start, endInclusive + 1);
      const len = endInclusive - start + 1;

      // Only record the block-aligned chunk fetches (ignore the 0-0 probe).
      const ifRange = headerValue(init, "If-Range");
      if (len !== 1) {
        chunkCalls += 1;
        seenChunkIfRanges.push(ifRange);
      }

      return new Response(body.buffer, {
        status: 206,
        headers: {
          "Accept-Ranges": "bytes",
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
});
