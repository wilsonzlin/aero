import test from "node:test";
import assert from "node:assert/strict";

import { OpfsLruChunkCache } from "../src/storage/remote/opfs_lru_chunk_cache.ts";

class MemFileSystemWritableFileStream {
  private parts: Uint8Array[] = [];
  private aborted = false;
  private readonly file: MemFileSystemFileHandle;

  constructor(file: MemFileSystemFileHandle, keepExistingData: boolean) {
    this.file = file;
    if (keepExistingData) {
      this.parts.push(file.data.slice());
    }
  }

  async write(chunk: unknown): Promise<void> {
    if (this.aborted) throw new Error("write after abort");

    if (typeof chunk === "string") {
      this.parts.push(new TextEncoder().encode(chunk));
      return;
    }

    if (chunk && typeof chunk === "object") {
      // Support the common `{ type: "write", data: ... }` form.
      const maybeParams = chunk as { type?: unknown; data?: unknown };
      if (maybeParams.type === "write" && maybeParams.data !== undefined) {
        await this.write(maybeParams.data);
        return;
      }
    }

    if (chunk instanceof Uint8Array) {
      this.parts.push(chunk.slice());
      return;
    }
    if (chunk instanceof ArrayBuffer) {
      this.parts.push(new Uint8Array(chunk));
      return;
    }
    if (ArrayBuffer.isView(chunk)) {
      this.parts.push(new Uint8Array(chunk.buffer, chunk.byteOffset, chunk.byteLength));
      return;
    }

    throw new Error(`unsupported write() chunk type: ${typeof chunk}`);
  }

  async close(): Promise<void> {
    if (this.aborted) return;
    const total = this.parts.reduce((sum, p) => sum + p.byteLength, 0);
    const out = new Uint8Array(total);
    let off = 0;
    for (const p of this.parts) {
      out.set(p, off);
      off += p.byteLength;
    }
    this.file.data = out;
    this.file.lastModified = Date.now();
  }

  async abort(): Promise<void> {
    this.aborted = true;
  }
}

class MemFileSystemFileHandle {
  readonly kind = "file" as const;
  readonly name: string;
  data: Uint8Array = new Uint8Array(0);
  lastModified = Date.now();

  constructor(name: string) {
    this.name = name;
  }

  async getFile(): Promise<File> {
    return new File([this.data], this.name, { lastModified: this.lastModified });
  }

  async createWritable(opts: { keepExistingData?: boolean } = {}): Promise<MemFileSystemWritableFileStream> {
    return new MemFileSystemWritableFileStream(this, opts.keepExistingData !== false);
  }
}

class MemFileSystemDirectoryHandle {
  readonly kind = "directory" as const;
  readonly name: string;
  private readonly children = new Map<string, MemFileSystemDirectoryHandle | MemFileSystemFileHandle>();

  constructor(name: string) {
    this.name = name;
  }

  async getDirectoryHandle(name: string, opts: { create?: boolean } = {}): Promise<MemFileSystemDirectoryHandle> {
    const existing = this.children.get(name);
    if (existing) {
      if (existing.kind !== "directory") throw new DOMException("Not a directory", "TypeMismatchError");
      return existing;
    }
    if (!opts.create) throw new DOMException("Not found", "NotFoundError");
    const dir = new MemFileSystemDirectoryHandle(name);
    this.children.set(name, dir);
    return dir;
  }

  async getFileHandle(name: string, opts: { create?: boolean } = {}): Promise<MemFileSystemFileHandle> {
    const existing = this.children.get(name);
    if (existing) {
      if (existing.kind !== "file") throw new DOMException("Not a file", "TypeMismatchError");
      return existing;
    }
    if (!opts.create) throw new DOMException("Not found", "NotFoundError");
    const file = new MemFileSystemFileHandle(name);
    this.children.set(name, file);
    return file;
  }

  async removeEntry(name: string, opts: { recursive?: boolean } = {}): Promise<void> {
    const existing = this.children.get(name);
    if (!existing) throw new DOMException("Not found", "NotFoundError");
    if (existing.kind === "directory") {
      if (!opts.recursive && existing.children.size > 0) {
        throw new DOMException("Directory not empty", "InvalidModificationError");
      }
    }
    this.children.delete(name);
  }

  async *entries(): AsyncIterableIterator<[string, MemFileSystemDirectoryHandle | MemFileSystemFileHandle]> {
    for (const entry of this.children.entries()) {
      yield entry;
    }
  }
}

function installOpfsMock(): MemFileSystemDirectoryHandle {
  const root = new MemFileSystemDirectoryHandle("opfs-root");
  // Node.js defines a getter-only `navigator`; add a `storage.getDirectory` method to it.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const nav = globalThis.navigator as any;
  nav.storage = {
    getDirectory: async () => root,
  };
  return root;
}

async function getDir(
  root: MemFileSystemDirectoryHandle,
  parts: string[],
  opts: { create: boolean },
): Promise<MemFileSystemDirectoryHandle> {
  let dir = root;
  for (const part of parts) {
    dir = await dir.getDirectoryHandle(part, { create: opts.create });
  }
  return dir;
}

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
