import test from "node:test";
import assert from "node:assert/strict";

import { OpfsLruChunkCache } from "../src/storage/remote/opfs_lru_chunk_cache.ts";
import { getDir, installOpfsMock } from "./opfs_mock.ts";

test("OpfsLruChunkCache", async (t) => {
  await t.test("insert/get roundtrip", async () => {
    installOpfsMock();
    const cache = await OpfsLruChunkCache.open({ cacheKey: "test", chunkSize: 4, maxBytes: 100 });

    const data = Uint8Array.from([1, 2, 3, 4]);
    const put = await cache.putChunk(0, data);
    assert.equal(put.stored, true);
    assert.deepEqual(put.evicted, []);

    const got = await cache.getChunk(0, 4);
    assert.ok(got);
    assert.deepEqual(Array.from(got), Array.from(data));

    const stats = await cache.getStats();
    assert.equal(stats.totalBytes, 4);
    assert.equal(stats.chunkCount, 1);
  });

  await t.test("eviction respects LRU order (hits update recency)", async () => {
    installOpfsMock();
    const cache = await OpfsLruChunkCache.open({ cacheKey: "test", chunkSize: 4, maxBytes: 8 });

    await cache.putChunk(0, Uint8Array.from([0, 0, 0, 0]));
    await cache.putChunk(1, Uint8Array.from([1, 1, 1, 1]));

    // Touch chunk 0 so chunk 1 becomes LRU.
    assert.ok(await cache.getChunk(0, 4));

    const put = await cache.putChunk(2, Uint8Array.from([2, 2, 2, 2]));
    assert.deepEqual(put.evicted, [1]);

    assert.equal(await cache.getChunk(1, 4), null);
    assert.ok(await cache.getChunk(0, 4));
    assert.ok(await cache.getChunk(2, 4));
  });

  await t.test("enforces maxBytes bound", async () => {
    installOpfsMock();
    const cache = await OpfsLruChunkCache.open({ cacheKey: "test", chunkSize: 4, maxBytes: 4 });

    await cache.putChunk(0, Uint8Array.from([0, 0, 0, 0]));
    await cache.putChunk(1, Uint8Array.from([1, 1, 1, 1]));

    assert.equal(await cache.getChunk(0, 4), null);
    assert.ok(await cache.getChunk(1, 4));

    const stats = await cache.getStats();
    assert.equal(stats.totalBytes, 4);
    assert.equal(stats.chunkCount, 1);
  });

  await t.test("recovery: missing chunk files + orphan tmp + orphan chunk files", async () => {
    const root = installOpfsMock();

    const cache = await OpfsLruChunkCache.open({ cacheKey: "test", chunkSize: 4, maxBytes: 100 });
    await cache.putChunk(0, Uint8Array.from([0, 0, 0, 0]));
    await cache.putChunk(1, Uint8Array.from([1, 1, 1, 1]));

    const chunksDir = await getDir(root, ["aero", "disks", "remote-cache", "test", "chunks"], { create: false });

    // Simulate a missing chunk file (index still says present).
    await chunksDir.removeEntry("1.bin");

    // Simulate an orphan chunk file that exists but isn't in the index.
    {
      const orphan = await chunksDir.getFileHandle("2.bin", { create: true });
      const writable = await orphan.createWritable({ keepExistingData: false });
      await writable.write(Uint8Array.from([2, 2, 2, 2]));
      await writable.close();
    }

    // Simulate an orphan temp file.
    {
      const tmp = await chunksDir.getFileHandle("3.tmp", { create: true });
      const writable = await tmp.createWritable({ keepExistingData: false });
      await writable.write(Uint8Array.from([3, 3, 3, 3]));
      await writable.close();
    }

    const reopened = await OpfsLruChunkCache.open({ cacheKey: "test", chunkSize: 4, maxBytes: 100 });

    assert.equal(await reopened.getChunk(1, 4), null);
    const gotOrphan = await reopened.getChunk(2, 4);
    assert.ok(gotOrphan);
    assert.deepEqual(Array.from(gotOrphan), [2, 2, 2, 2]);

    const names: string[] = [];
    for await (const [name] of chunksDir.entries()) names.push(name);
    assert.ok(!names.includes("3.tmp"));

    const stats = await reopened.getStats();
    assert.equal(stats.chunkCount, 2);
    assert.equal(stats.totalBytes, 8);
  });
});
