import "../../test/fake_indexeddb_auto.ts";

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { clearIdb, idbTxDone, openDiskManagerDb } from "./metadata";
import { IdbRemoteChunkCache, IdbRemoteChunkCacheQuotaError } from "./idb_remote_chunk_cache";

const CHUNK_SIZE = 512 * 1024;

function makeChunk(fill: number): Uint8Array {
  const out = new Uint8Array(CHUNK_SIZE);
  out.fill(fill & 0xff);
  return out;
}

describe("IdbRemoteChunkCache", () => {
  beforeEach(async () => {
    await clearIdb();
  });

  afterEach(async () => {
    // Ensure the next test gets a clean DB even if it opened connections.
    await clearIdb();
  });

  it("puts and gets chunks", async () => {
    const cache = await IdbRemoteChunkCache.open({
      cacheKey: "k",
      signature: {
        imageId: "img",
        version: "v1",
        etag: "e1",
        lastModified: null,
        sizeBytes: 4 * CHUNK_SIZE,
        chunkSize: CHUNK_SIZE,
      },
      cacheLimitBytes: null,
    });

    await cache.put(0, makeChunk(0x11));
    await cache.put(1, makeChunk(0x22));

    const hit = await cache.get(0);
    expect(hit).not.toBeNull();
    expect(hit!.byteLength).toBe(CHUNK_SIZE);
    expect(hit!.subarray(0, 8)).toEqual(new Uint8Array(8).fill(0x11));

    const miss = await cache.get(7);
    expect(miss).toBeNull();

    cache.close();
  });

  it("evicts least-recently-used chunks when exceeding cacheLimitBytes", async () => {
    const limit = 2 * CHUNK_SIZE;
    const cache = await IdbRemoteChunkCache.open({
      cacheKey: "k",
      signature: {
        imageId: "img",
        version: "v1",
        etag: "e1",
        lastModified: null,
        sizeBytes: 16 * CHUNK_SIZE,
        chunkSize: CHUNK_SIZE,
      },
      cacheLimitBytes: limit,
    });

    await cache.put(0, makeChunk(0x01));
    await cache.put(1, makeChunk(0x02));

    // Touch chunk 0 so chunk 1 becomes LRU.
    expect((await cache.get(0))!.subarray(0, 1)).toEqual(new Uint8Array([0x01]));

    await cache.put(2, makeChunk(0x03));

    expect(await cache.get(1)).toBeNull();
    expect((await cache.get(0))!.subarray(0, 1)).toEqual(new Uint8Array([0x01]));
    expect((await cache.get(2))!.subarray(0, 1)).toEqual(new Uint8Array([0x03]));

    const status = await cache.getStatus();
    expect(status.bytesUsed).toBeLessThanOrEqual(limit);

    cache.close();
  });

  it("invalidates cached chunks when the signature changes", async () => {
    const sig1 = {
      imageId: "img",
      version: "v1",
      etag: "e1",
      lastModified: null,
      sizeBytes: 4 * CHUNK_SIZE,
      chunkSize: CHUNK_SIZE,
    };
    const sig2 = {
      imageId: "img",
      version: "v1",
      etag: "e2",
      lastModified: null,
      sizeBytes: 4 * CHUNK_SIZE,
      chunkSize: CHUNK_SIZE,
    };

    const cache1 = await IdbRemoteChunkCache.open({ cacheKey: "k", signature: sig1 });
    await cache1.put(0, makeChunk(0xaa));
    cache1.close();

    const cache2 = await IdbRemoteChunkCache.open({ cacheKey: "k", signature: sig2 });
    expect(await cache2.get(0)).toBeNull();
    const status = await cache2.getStatus();
    expect(status.bytesUsed).toBe(0);
    cache2.close();
  });

  it("does not observe chunk records inherited from Object.prototype", async () => {
    const existingCacheKey = Object.getOwnPropertyDescriptor(Object.prototype, "cacheKey");
    const existingChunkIndex = Object.getOwnPropertyDescriptor(Object.prototype, "chunkIndex");
    const existingData = Object.getOwnPropertyDescriptor(Object.prototype, "data");
    if (
      (existingCacheKey && existingCacheKey.configurable === false) ||
      (existingChunkIndex && existingChunkIndex.configurable === false) ||
      (existingData && existingData.configurable === false)
    ) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    const cache = await IdbRemoteChunkCache.open({
      cacheKey: "k",
      signature: {
        imageId: "img",
        version: "v1",
        etag: "e1",
        lastModified: null,
        sizeBytes: 4 * CHUNK_SIZE,
        chunkSize: CHUNK_SIZE,
      },
      cacheLimitBytes: null,
    });

    try {
      Object.defineProperty(Object.prototype, "cacheKey", { value: "k", configurable: true });
      Object.defineProperty(Object.prototype, "chunkIndex", { value: 0, configurable: true });
      Object.defineProperty(Object.prototype, "data", { value: makeChunk(0x99).buffer, configurable: true });

      // Write a corrupt record with no own properties. `remote_chunks` keyPath extraction can
      // (depending on implementation) observe inherited properties; our read path must not.
      const db = await openDiskManagerDb();
      try {
        const tx = db.transaction(["remote_chunks"], "readwrite");
        tx.objectStore("remote_chunks").put({});
        await idbTxDone(tx);
      } finally {
        db.close();
      }

      expect(await cache.get(0)).toBeNull();
    } finally {
      cache.close();
      if (existingCacheKey) Object.defineProperty(Object.prototype, "cacheKey", existingCacheKey);
      else Reflect.deleteProperty(Object.prototype, "cacheKey");
      if (existingChunkIndex) Object.defineProperty(Object.prototype, "chunkIndex", existingChunkIndex);
      else Reflect.deleteProperty(Object.prototype, "chunkIndex");
      if (existingData) Object.defineProperty(Object.prototype, "data", existingData);
      else Reflect.deleteProperty(Object.prototype, "data");
    }
  });

  it("wraps quota errors during open as IdbRemoteChunkCacheQuotaError", async () => {
    const originalOpen = indexedDB.open.bind(indexedDB);
    (indexedDB as unknown as { open: typeof indexedDB.open }).open = ((..._args: any[]) => {
      const req: any = { result: null, error: new DOMException("quota exceeded", "QuotaExceededError") };
      queueMicrotask(() => req.onerror?.());
      return req;
    }) as unknown as typeof indexedDB.open;

    try {
      await expect(
        IdbRemoteChunkCache.open({
          cacheKey: "k",
          signature: {
            imageId: "img",
            version: "v1",
            etag: "e1",
            lastModified: null,
            sizeBytes: 4 * CHUNK_SIZE,
            chunkSize: CHUNK_SIZE,
          },
        }),
      ).rejects.toBeInstanceOf(IdbRemoteChunkCacheQuotaError);
    } finally {
      (indexedDB as unknown as { open: typeof indexedDB.open }).open = originalOpen;
    }
  });

  it("wraps Firefox quota errors during open as IdbRemoteChunkCacheQuotaError", async () => {
    const originalOpen = indexedDB.open.bind(indexedDB);
    (indexedDB as unknown as { open: typeof indexedDB.open }).open = ((..._args: any[]) => {
      const req: any = { result: null, error: new DOMException("quota reached", "NS_ERROR_DOM_QUOTA_REACHED") };
      queueMicrotask(() => req.onerror?.());
      return req;
    }) as unknown as typeof indexedDB.open;

    try {
      await expect(
        IdbRemoteChunkCache.open({
          cacheKey: "k",
          signature: {
            imageId: "img",
            version: "v1",
            etag: "e1",
            lastModified: null,
            sizeBytes: 4 * CHUNK_SIZE,
            chunkSize: CHUNK_SIZE,
          },
        }),
      ).rejects.toBeInstanceOf(IdbRemoteChunkCacheQuotaError);
    } finally {
      (indexedDB as unknown as { open: typeof indexedDB.open }).open = originalOpen;
    }
  });
});
