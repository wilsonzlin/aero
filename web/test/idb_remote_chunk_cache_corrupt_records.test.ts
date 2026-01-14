import test from "node:test";
import assert from "node:assert/strict";

// Install an IndexedDB implementation for Node-based unit tests.
import "./fake_indexeddb_auto.ts";

import { clearIdb, idbTxDone, openDiskManagerDb } from "../src/storage/metadata.ts";
import { IdbRemoteChunkCache } from "../src/storage/idb_remote_chunk_cache.ts";

const CHUNK_SIZE = 512 * 1024;

function signature() {
  return {
    imageId: "img",
    version: "v1",
    etag: "e1",
    lastModified: null,
    sizeBytes: 2 * CHUNK_SIZE,
    chunkSize: CHUNK_SIZE,
  };
}

test("IdbRemoteChunkCache: corrupt chunk records are treated as cache misses", async (t) => {
  await t.test("get() does not serve wrong-sized stored data", async () => {
    await clearIdb();
    const sig = signature();

    const db = await openDiskManagerDb();
    try {
      const tx = db.transaction(["remote_chunk_meta", "remote_chunks"], "readwrite");
      const metaStore = tx.objectStore("remote_chunk_meta");
      const chunksStore = tx.objectStore("remote_chunks");

      metaStore.put({
        cacheKey: "k",
        imageId: sig.imageId,
        version: sig.version,
        etag: sig.etag,
        lastModified: sig.lastModified,
        sizeBytes: sig.sizeBytes,
        chunkSize: sig.chunkSize,
        bytesUsed: 1,
        accessCounter: 0,
      });
      // Corrupt: chunk payload has the wrong length.
      chunksStore.put({
        cacheKey: "k",
        chunkIndex: 0,
        data: new ArrayBuffer(1),
        byteLength: 1,
        lastAccess: 0,
      });
      await idbTxDone(tx);
    } finally {
      db.close();
    }

    const cache = await IdbRemoteChunkCache.open({ cacheKey: "k", signature: sig, cacheLimitBytes: null });
    try {
      assert.equal(await cache.get(0), null);
      const status = await cache.getStatus();
      assert.equal(status.bytesUsed, 0);
    } finally {
      cache.close();
      await clearIdb();
    }
  });

  await t.test("put() overwriting corrupt byteLength does not poison bytesUsed", async () => {
    await clearIdb();
    const sig = signature();

    const db = await openDiskManagerDb();
    try {
      const tx = db.transaction(["remote_chunk_meta", "remote_chunks"], "readwrite");
      const metaStore = tx.objectStore("remote_chunk_meta");
      const chunksStore = tx.objectStore("remote_chunks");

      metaStore.put({
        cacheKey: "k",
        imageId: sig.imageId,
        version: sig.version,
        etag: sig.etag,
        lastModified: sig.lastModified,
        sizeBytes: sig.sizeBytes,
        chunkSize: sig.chunkSize,
        bytesUsed: CHUNK_SIZE,
        accessCounter: 1,
      });
      // Corrupt: `byteLength` stored as a string.
      chunksStore.put({
        cacheKey: "k",
        chunkIndex: 0,
        data: new Uint8Array(CHUNK_SIZE).fill(0x11).buffer,
        byteLength: "oops",
        lastAccess: 1,
      } as any);
      await idbTxDone(tx);
    } finally {
      db.close();
    }

    const cache = await IdbRemoteChunkCache.open({ cacheKey: "k", signature: sig, cacheLimitBytes: null });
    try {
      await cache.put(0, new Uint8Array(CHUNK_SIZE).fill(0x22));
      const status = await cache.getStatus();
      assert.ok(Number.isFinite(status.bytesUsed), `expected finite bytesUsed, got ${status.bytesUsed}`);
      assert.equal(status.bytesUsed, CHUNK_SIZE);
      const got = await cache.get(0);
      assert.ok(got);
      assert.equal(got[0], 0x22);
    } finally {
      cache.close();
      await clearIdb();
    }
  });
});

