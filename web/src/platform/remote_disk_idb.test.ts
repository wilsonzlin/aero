import "fake-indexeddb/auto";

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

describe("RemoteStreamingDisk (IndexedDB cache)", () => {
  // `RemoteStreamingDisk` treats `cacheLimitBytes: null` as "cache disabled".
  // For these tests we want caching enabled without eviction, so pick a large cap.
  const cacheLimitBytes = 1024 * 1024 * 1024;

  beforeEach(async () => {
    await clearIdb();
  });

  afterEach(async () => {
    await clearIdb();
  });

  it("caches fetched blocks in IndexedDB and reuses them on subsequent reads", async () => {
    const blockSize = 1024 * 1024;
    // NOTE: `RemoteStreamingDisk` treats `cacheLimitBytes=null` as "cache disabled".
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
});
