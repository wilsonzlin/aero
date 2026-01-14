import { afterEach, describe, expect, it } from "vitest";

import { installOpfsMock, getDir } from "../../../test/opfs_mock.ts";
import { OpfsLruChunkCache } from "./opfs_lru_chunk_cache";

let realNavigatorStorage: unknown = undefined;
let hadNavigatorStorage = false;

afterEach(() => {
  // Restore `navigator.storage` after OPFS mock tests.
  const nav = globalThis.navigator as unknown as { storage?: unknown };
  if (hadNavigatorStorage) {
    nav.storage = realNavigatorStorage;
  } else {
    delete nav.storage;
  }
  realNavigatorStorage = undefined;
  hadNavigatorStorage = false;
});

describe("OpfsLruChunkCache prototype pollution hardening", () => {
  it("does not accept required index.json fields inherited from Object.prototype (signature wipe)", async () => {
    const nav = globalThis.navigator as unknown as { storage?: unknown };
    realNavigatorStorage = nav.storage;
    hadNavigatorStorage = Object.prototype.hasOwnProperty.call(nav, "storage");

    const root = installOpfsMock();

    const cacheKey = "test-cache";
    const chunkSize = 4;
    const expectedSignature = { imageId: "img", version: "v1", etag: null, sizeBytes: 4, chunkSize };

    // Create a stale chunk file and an invalid/empty index.json.
    const baseDir = await getDir(root, ["aero", "disks", "remote-cache", cacheKey], { create: true });
    const chunksDir = await baseDir.getDirectoryHandle("chunks", { create: true });
    const chunkHandle = await chunksDir.getFileHandle("0.bin", { create: true });
    const chunkWritable = await chunkHandle.createWritable({ keepExistingData: false });
    await chunkWritable.write(new Uint8Array([1, 2, 3, 4]));
    await chunkWritable.close();

    const indexHandle = await baseDir.getFileHandle("index.json", { create: true });
    const indexWritable = await indexHandle.createWritable({ keepExistingData: false });
    await indexWritable.write("{}");
    await indexWritable.close();

    const existingVersion = Object.getOwnPropertyDescriptor(Object.prototype, "version");
    const existingChunkSize = Object.getOwnPropertyDescriptor(Object.prototype, "chunkSize");
    const existingAccessCounter = Object.getOwnPropertyDescriptor(Object.prototype, "accessCounter");
    const existingChunks = Object.getOwnPropertyDescriptor(Object.prototype, "chunks");
    const existingSignature = Object.getOwnPropertyDescriptor(Object.prototype, "signature");
    if (
      (existingVersion && existingVersion.configurable === false) ||
      (existingChunkSize && existingChunkSize.configurable === false) ||
      (existingAccessCounter && existingAccessCounter.configurable === false) ||
      (existingChunks && existingChunks.configurable === false) ||
      (existingSignature && existingSignature.configurable === false)
    ) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    try {
      // Pollute Object.prototype so an empty index.json could appear structurally valid and match
      // the expected signature. The cache must still treat the on-disk state as untrusted and wipe.
      Object.defineProperty(Object.prototype, "version", { value: 1, configurable: true });
      Object.defineProperty(Object.prototype, "chunkSize", { value: chunkSize, configurable: true });
      Object.defineProperty(Object.prototype, "accessCounter", { value: 0, configurable: true });
      Object.defineProperty(Object.prototype, "chunks", { value: {}, configurable: true });
      Object.defineProperty(Object.prototype, "signature", { value: expectedSignature, configurable: true });

      const cache = await OpfsLruChunkCache.open({ cacheKey, chunkSize, maxBytes: 1024, signature: expectedSignature });
      const indices = await cache.getChunkIndices();
      expect(indices).toEqual([]);
    } finally {
      if (existingVersion) Object.defineProperty(Object.prototype, "version", existingVersion);
      else Reflect.deleteProperty(Object.prototype, "version");
      if (existingChunkSize) Object.defineProperty(Object.prototype, "chunkSize", existingChunkSize);
      else Reflect.deleteProperty(Object.prototype, "chunkSize");
      if (existingAccessCounter) Object.defineProperty(Object.prototype, "accessCounter", existingAccessCounter);
      else Reflect.deleteProperty(Object.prototype, "accessCounter");
      if (existingChunks) Object.defineProperty(Object.prototype, "chunks", existingChunks);
      else Reflect.deleteProperty(Object.prototype, "chunks");
      if (existingSignature) Object.defineProperty(Object.prototype, "signature", existingSignature);
      else Reflect.deleteProperty(Object.prototype, "signature");
    }
  });
});
