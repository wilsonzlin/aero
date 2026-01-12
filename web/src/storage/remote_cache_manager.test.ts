import { describe, expect, it } from "vitest";

import {
  type RemoteCacheDirectoryHandle,
  type RemoteCacheFile,
  type RemoteCacheFileHandle,
  type RemoteCacheWritableFileStream,
  RemoteCacheManager,
  validateRemoteCacheMetaV1,
  remoteRangeDeliveryType,
} from "./remote_cache_manager";

class MemNotFoundError extends Error {
  override name = "NotFoundError";
}

class MemFile implements RemoteCacheFile {
  data: Uint8Array = new Uint8Array();

  get size(): number {
    return this.data.byteLength;
  }

  async text(): Promise<string> {
    return new TextDecoder().decode(this.data);
  }

  async arrayBuffer(): Promise<ArrayBuffer> {
    // Return a copy to match File.arrayBuffer() semantics.
    return this.data.slice().buffer;
  }
}

class MemWritable implements RemoteCacheWritableFileStream {
  private chunks: Uint8Array[] = [];
  private closed = false;

  constructor(
    private readonly file: MemFile,
    private readonly keepExistingData: boolean,
  ) {
    if (keepExistingData) {
      this.chunks.push(file.data.slice());
    }
  }

  async write(data: string | Uint8Array): Promise<void> {
    if (this.closed) throw new Error("writable already closed");
    if (typeof data === "string") {
      this.chunks.push(new TextEncoder().encode(data));
    } else {
      this.chunks.push(data);
    }
  }

  async close(): Promise<void> {
    if (this.closed) return;
    this.closed = true;
    const total = this.chunks.reduce((sum, c) => sum + c.byteLength, 0);
    const out = new Uint8Array(total);
    let off = 0;
    for (const c of this.chunks) {
      out.set(c, off);
      off += c.byteLength;
    }
    this.file.data = out;
  }
}

class MemFileHandle implements RemoteCacheFileHandle {
  constructor(private readonly file: MemFile) {}

  async getFile(): Promise<RemoteCacheFile> {
    return this.file;
  }

  async createWritable(options?: { keepExistingData?: boolean }): Promise<RemoteCacheWritableFileStream> {
    return new MemWritable(this.file, options?.keepExistingData === true);
  }
}

type MemEntry = { kind: "file"; file: MemFile } | { kind: "dir"; dir: MemDir };

class MemDir implements RemoteCacheDirectoryHandle {
  private readonly entriesMap = new Map<string, MemEntry>();

  async getDirectoryHandle(name: string, options?: { create?: boolean }): Promise<RemoteCacheDirectoryHandle> {
    const existing = this.entriesMap.get(name);
    if (existing) {
      if (existing.kind !== "dir") throw new Error("path is a file");
      return existing.dir;
    }
    if (!options?.create) throw new MemNotFoundError(`dir not found: ${name}`);
    const dir = new MemDir();
    this.entriesMap.set(name, { kind: "dir", dir });
    return dir;
  }

  async getFileHandle(name: string, options?: { create?: boolean }): Promise<RemoteCacheFileHandle> {
    const existing = this.entriesMap.get(name);
    if (existing) {
      if (existing.kind !== "file") throw new Error("path is a directory");
      return new MemFileHandle(existing.file);
    }
    if (!options?.create) throw new MemNotFoundError(`file not found: ${name}`);
    const file = new MemFile();
    this.entriesMap.set(name, { kind: "file", file });
    return new MemFileHandle(file);
  }

  async removeEntry(name: string, options?: { recursive?: boolean }): Promise<void> {
    const existing = this.entriesMap.get(name);
    if (!existing) throw new MemNotFoundError(`missing entry: ${name}`);
    if (existing.kind === "dir" && existing.dir.entriesMap.size > 0 && !options?.recursive) {
      throw new Error("directory not empty");
    }
    this.entriesMap.delete(name);
  }

  async *entries(): AsyncIterable<[string, RemoteCacheDirectoryHandle | RemoteCacheFileHandle]> {
    for (const [name, entry] of this.entriesMap) {
      if (entry.kind === "dir") yield [name, entry.dir];
      else yield [name, new MemFileHandle(entry.file)];
    }
  }
}

describe("RemoteCacheManager", () => {
  it("derives stable cache keys from {imageId, version, deliveryType}", async () => {
    const a = await RemoteCacheManager.deriveCacheKey({
      imageId: "img-1",
      version: "v1",
      deliveryType: remoteRangeDeliveryType(1024),
    });
    const b = await RemoteCacheManager.deriveCacheKey({
      imageId: "img-1",
      version: "v1",
      deliveryType: remoteRangeDeliveryType(1024),
    });
    const c = await RemoteCacheManager.deriveCacheKey({
      imageId: "img-1",
      version: "v2",
      deliveryType: remoteRangeDeliveryType(1024),
    });
    const d = await RemoteCacheManager.deriveCacheKey({
      imageId: "img-1",
      version: "v1",
      deliveryType: remoteRangeDeliveryType(2048),
    });
    expect(a).toBe(b);
    expect(a).not.toBe(c);
    expect(a).not.toBe(d);
  });

  it("roundtrips metadata and reports cache status", async () => {
    const root = new MemDir();
    const mgr = new RemoteCacheManager(root, { now: () => 1234 });
    const { cacheKey } = await mgr.openCache(
      { imageId: "img-1", version: "v1", deliveryType: remoteRangeDeliveryType(1024) },
      { chunkSizeBytes: 1024, validators: { sizeBytes: 10_000, etag: '"e1"', lastModified: "Wed, 01 Jan 2025 00:00:00 GMT" } },
    );

    await mgr.recordCachedRange(cacheKey, 0, 1024);
    await mgr.recordCachedRange(cacheKey, 2048, 3072);

    const status = await mgr.getCacheStatus(cacheKey);
    expect(status).not.toBeNull();
    expect(status?.etag).toBe('"e1"');
    expect(status?.sizeBytes).toBe(10_000);
    expect(status?.cachedBytes).toBe(2048);
    expect(status?.cachedChunks).toBe(2);
    expect(status?.cachedRanges).toEqual([
      { start: 0, end: 1024 },
      { start: 2048, end: 3072 },
    ]);
  });

  it("invalidates cache directories when remote validators change", async () => {
    const root = new MemDir();
    let now = 1000;
    const mgr = new RemoteCacheManager(root, { now: () => now });

    const parts = { imageId: "img-1", version: "v1", deliveryType: remoteRangeDeliveryType(1024) };
    const first = await mgr.openCache(parts, { chunkSizeBytes: 1024, validators: { sizeBytes: 10, etag: "a" } });
    expect(first.invalidated).toBe(false);

    // Simulate cached payload.
    const cacheDir = first.dir;
    const blocks = await cacheDir.getDirectoryHandle("blocks", { create: true });
    const dummy = await blocks.getFileHandle("0.bin", { create: true });
    const w = await dummy.createWritable({ keepExistingData: false });
    await w.write("hello");
    await w.close();

    // Reopen with different ETag => invalidate and clear directory.
    now = 2000;
    const second = await mgr.openCache(parts, { chunkSizeBytes: 1024, validators: { sizeBytes: 10, etag: "b" } });
    expect(second.invalidated).toBe(true);

    // The blocks directory should be gone after invalidation.
    await expect(second.dir.getDirectoryHandle("blocks", { create: false })).rejects.toHaveProperty("name", "NotFoundError");
    const status = await mgr.getCacheStatus(second.cacheKey);
    expect(status?.createdAtMs).toBe(2000);
    expect(status?.cachedRanges).toEqual([]);
  });

  it("treats oversized meta.json files as corrupt without attempting to read them", async () => {
    const root = new MemDir();
    const mgr = new RemoteCacheManager(root, { now: () => 1234 });
    const cacheKey = await RemoteCacheManager.deriveCacheKey({
      imageId: "img-1",
      version: "v1",
      deliveryType: remoteRangeDeliveryType(1024),
    });

    const cacheDir = (await root.getDirectoryHandle(cacheKey, { create: true })) as MemDir;

    class HugeFile extends MemFile {
      override get size(): number {
        return 64 * 1024 * 1024 + 1; // just over MAX_CACHE_META_BYTES
      }

      override async text(): Promise<string> {
        throw new Error("should not read oversized meta file");
      }
    }

    // Inject an oversized meta.json entry.
    (cacheDir as any).entriesMap.set("meta.json", { kind: "file", file: new HugeFile() });

    const meta = await mgr.readMeta(cacheKey);
    expect(meta).toBeNull();
  });

  it("rejects meta.json files with invalid chunkLastAccess entries", () => {
    const parsed = {
      version: 1,
      imageId: "img",
      imageVersion: "v1",
      deliveryType: remoteRangeDeliveryType(512),
      validators: { sizeBytes: 1024 },
      chunkSizeBytes: 512,
      createdAtMs: 0,
      lastAccessedAtMs: 0,
      accessCounter: 0,
      chunkLastAccess: { "0": -1 },
      cachedRanges: [],
    };
    expect(validateRemoteCacheMetaV1(parsed)).toBeNull();
  });
});
