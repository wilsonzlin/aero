import { afterEach, describe, expect, it } from "vitest";

import { OpfsLruChunkCache } from "./opfs_lru_chunk_cache";
import { getDir, installMemoryOpfs, MemoryDirectoryHandle, MemoryFileHandle } from "../../test_utils/memory_opfs";

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

  it("treats index.json files with non-numeric chunk keys as corrupt and rebuilds from disk", async () => {
    const root = new MemoryDirectoryHandle("root");
    restoreOpfs = installMemoryOpfs(root).restore;

    const cacheKey = "test";

    // Pre-create the expected directory structure and inject a corrupt index.json with a
    // non-numeric key (e.g. "__proto__").
    const aeroDir = await root.getDirectoryHandle("aero", { create: true });
    const disksDir = await aeroDir.getDirectoryHandle("disks", { create: true });
    const remoteCacheDir = await disksDir.getDirectoryHandle("remote-cache", { create: true });
    const cacheDir = await remoteCacheDir.getDirectoryHandle(cacheKey, { create: true });
    const chunksDir = await cacheDir.getDirectoryHandle("chunks", { create: true });

    const chunkHandle = await chunksDir.getFileHandle("0.bin", { create: true });
    const chunkWritable = await chunkHandle.createWritable({ keepExistingData: false });
    await chunkWritable.write(new Uint8Array([1, 2, 3, 4]));
    await chunkWritable.close();

    const chunks: Record<string, unknown> = {};
    Object.defineProperty(chunks, "0", { value: { byteLength: 4, lastAccess: 1 }, enumerable: true, configurable: true });
    Object.defineProperty(chunks, "__proto__", {
      value: { byteLength: 123, lastAccess: 0 },
      enumerable: true,
      configurable: true,
    });
    const index = { version: 1, chunkSize: 4, accessCounter: 1, chunks };
    const indexHandle = await cacheDir.getFileHandle("index.json", { create: true });
    const indexWritable = await indexHandle.createWritable({ keepExistingData: false });
    await indexWritable.write(JSON.stringify(index));
    await indexWritable.close();

    const cache = await OpfsLruChunkCache.open({ cacheKey, chunkSize: 4, maxBytes: 1024 });
    await expect(cache.getChunk(0, 4)).resolves.toEqual(new Uint8Array([1, 2, 3, 4]));
    await expect(cache.getChunkIndices()).resolves.toEqual([0]);

    const stats = await cache.getStats();
    expect(stats.totalBytes).toBe(4);
    expect(stats.chunkCount).toBe(1);
  });

  it("treats index.json files with non-canonical numeric chunk keys as corrupt and rebuilds from disk", async () => {
    const root = new MemoryDirectoryHandle("root");
    restoreOpfs = installMemoryOpfs(root).restore;

    const cacheKey = "test";

    const aeroDir = await root.getDirectoryHandle("aero", { create: true });
    const disksDir = await aeroDir.getDirectoryHandle("disks", { create: true });
    const remoteCacheDir = await disksDir.getDirectoryHandle("remote-cache", { create: true });
    const cacheDir = await remoteCacheDir.getDirectoryHandle(cacheKey, { create: true });
    const chunksDir = await cacheDir.getDirectoryHandle("chunks", { create: true });

    // Write two chunks on disk.
    {
      const h0 = await chunksDir.getFileHandle("0.bin", { create: true });
      const w0 = await h0.createWritable({ keepExistingData: false });
      await w0.write(new Uint8Array([1, 2, 3, 4]));
      await w0.close();
      const h1 = await chunksDir.getFileHandle("1.bin", { create: true });
      const w1 = await h1.createWritable({ keepExistingData: false });
      await w1.write(new Uint8Array([5, 6, 7, 8]));
      await w1.close();
    }

    // Corrupt index.json: uses a leading-zero key ("01") which is not a canonical `String(index)`
    // encoding and can otherwise create duplicate metadata entries.
    const index = {
      version: 1,
      chunkSize: 4,
      accessCounter: 2,
      chunks: {
        "0": { byteLength: 4, lastAccess: 1 },
        "01": { byteLength: 4, lastAccess: 2 },
      },
    };
    const indexHandle = await cacheDir.getFileHandle("index.json", { create: true });
    const indexWritable = await indexHandle.createWritable({ keepExistingData: false });
    await indexWritable.write(JSON.stringify(index));
    await indexWritable.close();

    const cache = await OpfsLruChunkCache.open({ cacheKey, chunkSize: 4, maxBytes: 1024 });
    await expect(cache.getChunkIndices()).resolves.toEqual([0, 1]);
    await expect(cache.getChunk(0, 4)).resolves.toEqual(new Uint8Array([1, 2, 3, 4]));
    await expect(cache.getChunk(1, 4)).resolves.toEqual(new Uint8Array([5, 6, 7, 8]));
  });

  it("does not allow Object.prototype pollution to suppress index reconciliation", async () => {
    const root = new MemoryDirectoryHandle("root");
    restoreOpfs = installMemoryOpfs(root).restore;

    const cacheKey = "test";

    const aeroDir = await root.getDirectoryHandle("aero", { create: true });
    const disksDir = await aeroDir.getDirectoryHandle("disks", { create: true });
    const remoteCacheDir = await disksDir.getDirectoryHandle("remote-cache", { create: true });
    const cacheDir = await remoteCacheDir.getDirectoryHandle(cacheKey, { create: true });
    const chunksDir = await cacheDir.getDirectoryHandle("chunks", { create: true });

    // Write a single chunk on disk.
    const chunkHandle = await chunksDir.getFileHandle("0.bin", { create: true });
    const chunkWritable = await chunkHandle.createWritable({ keepExistingData: false });
    await chunkWritable.write(new Uint8Array([1, 2, 3, 4]));
    await chunkWritable.close();

    // Write a valid but empty index.json so reconciliation must scan the filesystem.
    const index = { version: 1, chunkSize: 4, accessCounter: 0, chunks: {} };
    const indexHandle = await cacheDir.getFileHandle("index.json", { create: true });
    const indexWritable = await indexHandle.createWritable({ keepExistingData: false });
    await indexWritable.write(JSON.stringify(index));
    await indexWritable.close();

    const existing = Object.getOwnPropertyDescriptor(Object.prototype, "0");
    if (existing && existing.configurable === false) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    try {
      // Simulate prototype pollution with a numeric key that would otherwise interfere with
      // `chunks[key]` lookups.
      Object.defineProperty(Object.prototype, "0", {
        value: { byteLength: 999, lastAccess: 0 },
        configurable: true,
        writable: true,
      });

      const cache = await OpfsLruChunkCache.open({ cacheKey, chunkSize: 4, maxBytes: 1024 });
      await expect(cache.getChunkIndices()).resolves.toEqual([0]);
      await expect(cache.getChunk(0, 4)).resolves.toEqual(new Uint8Array([1, 2, 3, 4]));
    } finally {
      if (existing) Object.defineProperty(Object.prototype, "0", existing);
      else Reflect.deleteProperty(Object.prototype, "0");
    }
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

  it("pre-evicts before writes so maxBytes never transiently overflows", async () => {
    const root = new MemoryDirectoryHandle("root");
    restoreOpfs = installMemoryOpfs(root).restore;

    const cacheKey = "test";
    const chunkSize = 4;
    const maxBytes = chunkSize;

    const originalCreateWritable = MemoryFileHandle.prototype.createWritable;
    let quotaExceededCount = 0;
    (MemoryFileHandle.prototype as unknown as { createWritable: unknown }).createWritable = async function (
      this: MemoryFileHandle,
      options?: { keepExistingData?: boolean },
    ) {
      const inner = await originalCreateWritable.call(this, options);
      const fileName = this.name;
      if (!fileName.endsWith(".bin")) return inner;

      let stagedBytes = 0;
      return {
        write: async (data: string | Uint8Array) => {
          stagedBytes += typeof data === "string" ? new TextEncoder().encode(data).byteLength : data.byteLength;
          return await (inner as unknown as { write: (data: string | Uint8Array) => Promise<void> }).write(data);
        },
        close: async () => {
          const chunksDir = await getDir(root, ["aero", "disks", "remote-cache", cacheKey, "chunks"]);
          let usedBytes = 0;
          for await (const [name, handle] of chunksDir.entries()) {
            if (handle.kind !== "file") continue;
            if (!name.endsWith(".bin")) continue;
            if (name === fileName) continue;
            const file = await (handle as unknown as { getFile: () => Promise<{ size: number }> }).getFile();
            usedBytes += file.size;
          }
          if (usedBytes + stagedBytes > maxBytes) {
            quotaExceededCount += 1;
            throw new DOMException("Quota exceeded", "QuotaExceededError");
          }
          return await (inner as unknown as { close: () => Promise<void> }).close();
        },
        abort: async (reason?: unknown) => {
          return await (inner as unknown as { abort: (reason?: unknown) => Promise<void> }).abort(reason);
        },
      };
    };

    try {
      const cache = await OpfsLruChunkCache.open({ cacheKey, chunkSize, maxBytes });
      await expect(cache.putChunk(0, new Uint8Array(chunkSize).fill(0))).resolves.toMatchObject({ stored: true });
      await expect(cache.putChunk(1, new Uint8Array(chunkSize).fill(1))).resolves.toMatchObject({ stored: true });
      await expect(cache.putChunk(2, new Uint8Array(chunkSize).fill(2))).resolves.toMatchObject({ stored: true });

      expect(quotaExceededCount).toBe(0);

      const stats = await cache.getStats();
      expect(stats.totalBytes).toBe(chunkSize);
      expect(stats.chunkCount).toBe(1);

      await expect(cache.getChunkIndices()).resolves.toEqual([2]);
    } finally {
      (MemoryFileHandle.prototype as unknown as { createWritable: unknown }).createWritable = originalCreateWritable;
    }
  });

  it("treats QuotaExceededError writes as best-effort (non-fatal) and keeps index consistent", async () => {
    const root = new MemoryDirectoryHandle("root");
    restoreOpfs = installMemoryOpfs(root).restore;

    const cacheKey = "test";
    const cache = await OpfsLruChunkCache.open({ cacheKey, chunkSize: 4, maxBytes: 1024 });
    await cache.putChunk(0, new Uint8Array([0, 0, 0, 0]));

    const originalCreateWritable = MemoryFileHandle.prototype.createWritable;
    (MemoryFileHandle.prototype as unknown as { createWritable: unknown }).createWritable = async function (
      this: MemoryFileHandle,
      options?: { keepExistingData?: boolean },
    ) {
      const fileName = this.name;
      if (fileName.endsWith(".bin")) {
        return {
          write: async () => {
            throw new DOMException("Quota exceeded", "QuotaExceededError");
          },
          close: async () => undefined,
          abort: async () => undefined,
        };
      }
      return await originalCreateWritable.call(this, options);
    };

    let result: { stored: boolean; evicted: number[]; quotaExceeded: boolean } | null = null;
    try {
      result = await cache.putChunk(1, new Uint8Array([1, 1, 1, 1]));
    } finally {
      (MemoryFileHandle.prototype as unknown as { createWritable: unknown }).createWritable = originalCreateWritable;
    }

    expect(result).not.toBeNull();
    expect(result!.stored).toBe(false);
    expect(result!.quotaExceeded).toBe(true);

    // The cache may evict entries to try to make room, but it must not end up with a partially
    // cached chunk or a corrupt index.
    await expect(cache.getChunk(1, 4)).resolves.toBeNull();
    await expect(cache.getChunkIndices()).resolves.not.toContain(1);

    // Ensure the failed chunk did not leave an orphan file behind.
    const chunksDir = await getDir(root, ["aero", "disks", "remote-cache", cacheKey, "chunks"]);
    const chunkFiles: string[] = [];
    for await (const [name, handle] of chunksDir.entries()) {
      if (handle.kind === "file") chunkFiles.push(name);
    }
    expect(chunkFiles).not.toContain("1.bin");

    // Caller can continue using the cache after a quota failure.
    await expect(cache.putChunk(2, new Uint8Array([2, 2, 2, 2]))).resolves.toMatchObject({ stored: true });
    await cache.flush();
    const reopened = await OpfsLruChunkCache.open({ cacheKey, chunkSize: 4, maxBytes: 1024 });
    await expect(reopened.getChunk(2, 4)).resolves.toEqual(new Uint8Array([2, 2, 2, 2]));
  });

  it("treats QuotaExceededError index.json writes as best-effort (non-fatal) and keeps index consistent", async () => {
    const root = new MemoryDirectoryHandle("root");
    restoreOpfs = installMemoryOpfs(root).restore;

    const cacheKey = "test";
    const cache = await OpfsLruChunkCache.open({ cacheKey, chunkSize: 4, maxBytes: 1024 });
    await cache.putChunk(0, new Uint8Array([0, 0, 0, 0]));

    const originalCreateWritable = MemoryFileHandle.prototype.createWritable;
    (MemoryFileHandle.prototype as unknown as { createWritable: unknown }).createWritable = async function (
      this: MemoryFileHandle,
      options?: { keepExistingData?: boolean },
    ) {
      const fileName = this.name;
      if (fileName === "index.json") {
        return {
          write: async () => {
            throw new DOMException("Quota exceeded", "QuotaExceededError");
          },
          close: async () => undefined,
          abort: async () => undefined,
        };
      }
      return await originalCreateWritable.call(this, options);
    };

    let result: { stored: boolean; evicted: number[]; quotaExceeded: boolean } | null = null;
    try {
      result = await cache.putChunk(1, new Uint8Array([1, 1, 1, 1]));
    } finally {
      (MemoryFileHandle.prototype as unknown as { createWritable: unknown }).createWritable = originalCreateWritable;
    }

    expect(result).not.toBeNull();
    expect(result!.stored).toBe(false);
    expect(result!.quotaExceeded).toBe(true);

    // The cache may evict entries to try to make room, but it must not end up with a partially
    // cached chunk or a corrupt index.
    await expect(cache.getChunk(1, 4)).resolves.toBeNull();
    await expect(cache.getChunkIndices()).resolves.not.toContain(1);

    // Ensure the failed chunk did not leave an orphan file behind.
    const chunksDir = await getDir(root, ["aero", "disks", "remote-cache", cacheKey, "chunks"]);
    const chunkFiles: string[] = [];
    for await (const [name, handle] of chunksDir.entries()) {
      if (handle.kind === "file") chunkFiles.push(name);
    }
    expect(chunkFiles).not.toContain("1.bin");

    // Caller can continue using the cache after a quota failure.
    await expect(cache.putChunk(2, new Uint8Array([2, 2, 2, 2]))).resolves.toMatchObject({ stored: true });
    await cache.flush();
    const reopened = await OpfsLruChunkCache.open({ cacheKey, chunkSize: 4, maxBytes: 1024 });
    await expect(reopened.getChunk(2, 4)).resolves.toEqual(new Uint8Array([2, 2, 2, 2]));
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
    const h = indexHandle as unknown as {
      getFile: () => Promise<{ size: number; text: () => Promise<string>; arrayBuffer: () => Promise<ArrayBuffer> }>;
    };
    h.getFile = async () => ({
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
