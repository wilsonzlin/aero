import { afterEach, describe, expect, it } from "vitest";

import { OpfsLruChunkCache } from "./opfs_lru_chunk_cache";
import { getDir, installMemoryOpfs, MemoryDirectoryHandle } from "../../test_utils/memory_opfs";

let restoreOpfs: (() => void) | null = null;

afterEach(() => {
  restoreOpfs?.();
  restoreOpfs = null;
});

describe("OpfsLruChunkCache", () => {
  it("returns hits and misses", async () => {
    const root = new MemoryDirectoryHandle("root");
    restoreOpfs = installMemoryOpfs(root).restore;

    const cache = await OpfsLruChunkCache.open({ cacheKey: "test", chunkSize: 4, maxBytes: 1024 });
    await expect(cache.getChunk(0, 4)).resolves.toBeNull();

    await cache.putChunk(0, new Uint8Array([1, 2, 3, 4]));
    await expect(cache.getChunk(0, 4)).resolves.toEqual(new Uint8Array([1, 2, 3, 4]));

    const stats = await cache.getStats();
    expect(stats.totalBytes).toBe(4);
    expect(stats.chunkCount).toBe(1);
    expect(stats.maxBytes).toBe(1024);
  });

  it("evicts least-recently-used chunks when over the limit", async () => {
    const root = new MemoryDirectoryHandle("root");
    restoreOpfs = installMemoryOpfs(root).restore;

    const cache = await OpfsLruChunkCache.open({ cacheKey: "test", chunkSize: 4, maxBytes: 8 });
    await cache.putChunk(1, new Uint8Array([1, 1, 1, 1]));
    await cache.putChunk(2, new Uint8Array([2, 2, 2, 2]));

    // Touch chunk 1 so chunk 2 becomes LRU.
    await expect(cache.getChunk(1, 4)).resolves.toEqual(new Uint8Array([1, 1, 1, 1]));

    const put = await cache.putChunk(3, new Uint8Array([3, 3, 3, 3]));
    expect(put.evicted).toEqual([2]);

    await expect(cache.getChunk(2, 4)).resolves.toBeNull();
    await expect(cache.getChunk(1, 4)).resolves.toEqual(new Uint8Array([1, 1, 1, 1]));
    await expect(cache.getChunk(3, 4)).resolves.toEqual(new Uint8Array([3, 3, 3, 3]));

    const stats = await cache.getStats();
    expect(stats.totalBytes).toBe(8);
    expect(stats.chunkCount).toBe(2);
  });

  it("heals metadata when a chunk file is missing", async () => {
    const root = new MemoryDirectoryHandle("root");
    restoreOpfs = installMemoryOpfs(root).restore;

    const cacheKey = "test";
    const cache = await OpfsLruChunkCache.open({ cacheKey, chunkSize: 4, maxBytes: 1024 });
    await cache.putChunk(4, new Uint8Array([4, 4, 4, 4]));
    await cache.putChunk(5, new Uint8Array([5, 5, 5, 5]));

    // Simulate an external deletion: remove the chunk file but keep index.json.
    const chunksDir = await getDir(root, ["aero", "disks", "remote-cache", cacheKey, "chunks"]);
    await chunksDir.removeEntry("4.bin");

    const reopened = await OpfsLruChunkCache.open({ cacheKey, chunkSize: 4, maxBytes: 1024 });
    await expect(reopened.getChunk(4, 4)).resolves.toBeNull();
    await expect(reopened.getChunk(5, 4)).resolves.toEqual(new Uint8Array([5, 5, 5, 5]));

    const stats = await reopened.getStats();
    expect(stats.totalBytes).toBe(4);
    expect(stats.chunkCount).toBe(1);
  });

  it("respects maxBytes by evicting older entries", async () => {
    const root = new MemoryDirectoryHandle("root");
    restoreOpfs = installMemoryOpfs(root).restore;

    const cache = await OpfsLruChunkCache.open({ cacheKey: "test", chunkSize: 6, maxBytes: 10 });
    await cache.putChunk(0, new Uint8Array(6).fill(1));
    await cache.putChunk(1, new Uint8Array(6).fill(2));

    await expect(cache.getChunk(0, 6)).resolves.toBeNull();
    await expect(cache.getChunk(1, 6)).resolves.toEqual(new Uint8Array(6).fill(2));

    const stats = await cache.getStats();
    expect(stats.totalBytes).toBe(6);
    expect(stats.chunkCount).toBe(1);
  });

  it("treats oversized index.json files as corrupt without attempting to read them", async () => {
    const root = new MemoryDirectoryHandle("root");
    restoreOpfs = installMemoryOpfs(root).restore;

    const cacheKey = "test";
    // Pre-create the directory structure so we can inject a fake index.json before opening.
    const aeroDir = await root.getDirectoryHandle("aero", { create: true });
    const disksDir = await aeroDir.getDirectoryHandle("disks", { create: true });
    const remoteDir = await disksDir.getDirectoryHandle("remote-cache", { create: true });
    const baseDir = await remoteDir.getDirectoryHandle(cacheKey, { create: true });
    const indexHandle = await baseDir.getFileHandle("index.json", { create: true });

    // Inject a file object that reports a huge size and throws if read. The cache should
    // treat it as corrupt based on size alone.
    (indexHandle as any).getFile = async () => ({
      size: 64 * 1024 * 1024 + 1,
      async text() {
        throw new Error("should not read oversized index.json");
      },
      async arrayBuffer() {
        throw new Error("should not read oversized index.json");
      },
    });

    await expect(OpfsLruChunkCache.open({ cacheKey, chunkSize: 4, maxBytes: 1024 })).resolves.toBeDefined();
  });
});
