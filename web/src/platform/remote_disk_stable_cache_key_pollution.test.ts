import { describe, expect, it } from "vitest";

import { stableCacheKey } from "./remote_disk";
import { RANGE_STREAM_CHUNK_SIZE } from "../storage/chunk_sizes";

describe("stableCacheKey", () => {
  it("ignores inherited RemoteDiskOptions fields (prototype pollution)", async () => {
    const url = "https://example.invalid/disk.img";
    const expected = await stableCacheKey(url, { blockSize: RANGE_STREAM_CHUNK_SIZE });

    const existing = Object.getOwnPropertyDescriptor(Object.prototype, "blockSize");
    if (existing && existing.configurable === false) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    try {
      Object.defineProperty(Object.prototype, "blockSize", { value: 512, configurable: true, writable: true });
      const actual = await stableCacheKey(url);
      expect(actual).toBe(expected);
    } finally {
      if (existing) Object.defineProperty(Object.prototype, "blockSize", existing);
      else delete (Object.prototype as any).blockSize;
    }
  });
});

