import { afterEach, describe, expect, it } from "vitest";

import { installOpfsMock, getDir, MemFileSystemFileHandle } from "../../test/opfs_mock.ts";
import type { RemoteCacheMetaV1 } from "./remote_cache_manager";
import { RemoteCacheManager } from "./remote_cache_manager";

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

describe("RemoteCacheManager OPFS createWritable fallback", () => {
  it("truncates meta.json when createWritable options are unsupported", async () => {
    const nav = globalThis.navigator as unknown as { storage?: unknown };
    realNavigatorStorage = nav.storage;
    hadNavigatorStorage = Object.prototype.hasOwnProperty.call(nav, "storage");

    const root = installOpfsMock();

    const manager = await RemoteCacheManager.openOpfs({ now: () => 0 });
    const cacheKey = "test-cache";

    const baseMeta: Omit<RemoteCacheMetaV1, "chunkLastAccess" | "cachedRanges"> = {
      version: 1,
      imageId: "image",
      imageVersion: "v1",
      deliveryType: "range",
      validators: { sizeBytes: 16, etag: "etag", lastModified: "lm" },
      chunkSizeBytes: 4,
      createdAtMs: 0,
      lastAccessedAtMs: 0,
      accessCounter: 0,
    };

    const metaLong: RemoteCacheMetaV1 = {
      ...baseMeta,
      chunkLastAccess: { "0": 1, "1": 2, "2": 3, "3": 4, "4": 5 },
      cachedRanges: [{ start: 0, end: 4 }],
    };
    const metaShort: RemoteCacheMetaV1 = {
      ...baseMeta,
      chunkLastAccess: {},
      cachedRanges: [],
    };

    // First write succeeds with the options bag.
    await manager.writeMeta(cacheKey, metaLong);

    // Simulate an implementation that throws if `createWritable` receives options, but succeeds
    // with the default signature.
    const originalCreateWritable = MemFileSystemFileHandle.prototype.createWritable;
    (MemFileSystemFileHandle.prototype as unknown as { createWritable: unknown }).createWritable = async function (
      this: MemFileSystemFileHandle,
      ...args: Parameters<typeof originalCreateWritable>
    ) {
      if (this.name === "meta.json" && args.length > 0) {
        throw new Error("synthetic createWritable options not supported");
      }
      return await originalCreateWritable.call(this, ...args);
    };

    try {
      await manager.writeMeta(cacheKey, metaShort);
    } finally {
      (MemFileSystemFileHandle.prototype as unknown as { createWritable: unknown }).createWritable = originalCreateWritable;
    }

    const cacheDir = await getDir(root, ["aero", "disks", "remote-cache", cacheKey], { create: false });
    const file = await (await cacheDir.getFileHandle("meta.json", { create: false })).getFile();
    const text = await file.text();
    expect(text).toBe(JSON.stringify(metaShort, null, 2));
  });
});
