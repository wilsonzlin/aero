import { afterEach, describe, expect, it } from "vitest";

import { installOpfsMock, getDir, MemFileSystemFileHandle } from "../../../test/opfs_mock.ts";
import { OpfsLruChunkCache } from "./opfs_lru_chunk_cache";

let realNavigatorStorage: unknown = undefined;
let hadNavigatorStorage = false;

afterEach(() => {
  // Restore `navigator.storage` after OPFS mock tests.
  const nav = globalThis.navigator as unknown as { storage?: unknown };
  if (hadNavigatorStorage) {
    nav.storage = realNavigatorStorage;
  } else {
    Reflect.deleteProperty(nav, "storage");
  }
  realNavigatorStorage = undefined;
  hadNavigatorStorage = false;
});

describe("OpfsLruChunkCache OPFS createWritable fallback", () => {
  it("truncates index.json when createWritable options are unsupported", async () => {
    const nav = globalThis.navigator as unknown as { storage?: unknown };
    realNavigatorStorage = nav.storage;
    hadNavigatorStorage = Object.prototype.hasOwnProperty.call(nav, "storage");

    const root = installOpfsMock();

    const cacheKey = "test-cache";
    const cache = await OpfsLruChunkCache.open({ cacheKey, chunkSize: 4, maxBytes: 1024 });
    await cache.putChunk(0, new Uint8Array([0, 0, 0, 0]));

    // Simulate an implementation that throws if `createWritable` receives options, but succeeds
    // with the default signature. This forces the cache's metadata persistence to take the
    // fallback path, which must still truncate index.json to avoid appending/corrupting JSON.
    const originalCreateWritable = MemFileSystemFileHandle.prototype.createWritable;
    (MemFileSystemFileHandle.prototype as unknown as { createWritable: unknown }).createWritable = async function (
      this: MemFileSystemFileHandle,
      ...args: Parameters<typeof originalCreateWritable>
    ) {
      if (this.name === "index.json" && args.length > 0) {
        throw new Error("synthetic createWritable options not supported");
      }
      return await originalCreateWritable.call(this, ...args);
    };

    try {
      await cache.putChunk(1, new Uint8Array([1, 1, 1, 1]));
    } finally {
      (MemFileSystemFileHandle.prototype as unknown as { createWritable: unknown }).createWritable = originalCreateWritable;
    }

    const baseDir = await getDir(root, ["aero", "disks", "remote-cache", cacheKey], { create: false });
    const file = await (await baseDir.getFileHandle("index.json", { create: false })).getFile();
    const parsed = JSON.parse(await file.text()) as { chunks?: Record<string, { byteLength?: number }> };
    expect(parsed.chunks?.["0"]?.byteLength).toBe(4);
    expect(parsed.chunks?.["1"]?.byteLength).toBe(4);
  });
});
