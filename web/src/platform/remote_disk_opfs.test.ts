import { afterEach, describe, expect, it } from "vitest";

import { RemoteStreamingDisk } from "./remote_disk";
import { remoteRangeDeliveryType, RemoteCacheManager } from "../storage/remote_cache_manager";
import { getDir, installMemoryOpfs, MemoryDirectoryHandle } from "../test_utils/memory_opfs";

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

let restoreOpfs: (() => void) | null = null;
let restoreFetch: (() => void) | null = null;

afterEach(async () => {
  restoreFetch?.();
  restoreFetch = null;
  restoreOpfs?.();
  restoreOpfs = null;
});

describe("RemoteStreamingDisk (OPFS chunk cache)", () => {
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
});
