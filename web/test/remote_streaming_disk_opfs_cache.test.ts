import test from "node:test";
import assert from "node:assert/strict";

import { RemoteStreamingDisk } from "../src/platform/remote_disk.ts";
import { installOpfsMock } from "./opfs_mock.ts";

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
          "Cache-Control": "no-transform",
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
          "Cache-Control": "no-transform",
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
        "Cache-Control": "no-transform",
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

test("RemoteStreamingDisk (OPFS cache)", async (t) => {
  await t.test("caches fetched blocks in OPFS and reuses them on subsequent reads", async () => {
    installOpfsMock();
    const blockSize = 512;
    // NOTE: `RemoteStreamingDisk` treats `cacheLimitBytes=0` as "cache disabled".
    // Use a positive limit here so the OPFS cache is enabled.
    const cacheLimitBytes = blockSize * 8;
    const image = makeTestImage(blockSize * 3);
    const mock = installMockRangeFetch(image, { etag: '"e1"' });

    const disk = await RemoteStreamingDisk.open("https://example.test/disk.img", {
      blockSize,
      cacheBackend: "opfs",
      cacheLimitBytes,
      prefetchSequentialBlocks: 0,
      credentials: "omit",
    });

    const before = mock.stats.chunkRangeCalls;
    const first = await disk.read(0, 2);
    assert.deepEqual(Array.from(first), Array.from(image.subarray(0, 2)));
    assert.equal(mock.stats.chunkRangeCalls, before + 1);

    const second = await disk.read(0, 2);
    assert.deepEqual(Array.from(second), Array.from(image.subarray(0, 2)));
    assert.equal(mock.stats.chunkRangeCalls, before + 1);

    await disk.close();
    mock.restore();
  });

  await t.test("evicts least-recently-used blocks when cacheLimitBytes is exceeded", async () => {
    installOpfsMock();
    const blockSize = 512;
    const image = makeTestImage(blockSize * 3);
    const mock = installMockRangeFetch(image, { etag: '"e1"' });

    const disk = await RemoteStreamingDisk.open("https://example.test/disk.img", {
      blockSize,
      cacheBackend: "opfs",
      cacheLimitBytes: blockSize * 2,
      prefetchSequentialBlocks: 0,
      credentials: "omit",
    });
    const base = mock.stats.chunkRangeCalls;

    // Cache blocks 0 and 1.
    await disk.read(0, 1);
    await disk.read(blockSize, 1);
    assert.equal(mock.stats.chunkRangeCalls, base + 2);

    // Touch block 0 so block 1 becomes LRU.
    await disk.read(0, 1);
    assert.equal(mock.stats.chunkRangeCalls, base + 2);

    // Fetch block 2; should evict block 1.
    await disk.read(blockSize * 2, 1);
    assert.equal(mock.stats.chunkRangeCalls, base + 3);

    // Block 0 should still be cached.
    await disk.read(0, 1);
    assert.equal(mock.stats.chunkRangeCalls, base + 3);

    // Block 1 should have been evicted and will be refetched.
    await disk.read(blockSize, 1);
    assert.equal(mock.stats.chunkRangeCalls, base + 4);

    await disk.close();
    mock.restore();
  });
});
