import { describe, expect, it } from "vitest";

import type { RemoteRangeDiskMetadataStore, RemoteRangeDiskSparseCacheFactory } from "./remote_range_disk";
import { RemoteRangeDisk } from "./remote_range_disk";
import { MemorySparseDisk } from "./memory_sparse_disk";
import { OpfsCowDisk } from "./opfs_cow";
import { remoteRangeDeliveryType } from "./remote_cache_manager";

function createRangeFetch(data: Uint8Array<ArrayBuffer>): { fetch: typeof fetch; getCalls: () => number } {
  let calls = 0;
  const toArrayBuffer = (bytes: Uint8Array): ArrayBuffer => {
    const buf = new ArrayBuffer(bytes.byteLength);
    new Uint8Array(buf).set(bytes);
    return buf;
  };
  const fetcher: typeof fetch = async (_url, init) => {
    const method = String(init?.method || "GET").toUpperCase();
    const headers = init?.headers;
    const rangeHeader =
      headers instanceof Headers
        ? headers.get("Range") || undefined
        : typeof headers === "object" && headers
          ? ((headers as any).Range as string | undefined) ?? ((headers as any).range as string | undefined)
          : undefined;

    if (method === "HEAD") {
      return new Response(null, { status: 200, headers: { "Content-Length": String(data.byteLength) } });
    }

    calls += 1;

    if (rangeHeader) {
      const m = /^bytes=(\d+)-(\d+)$/.exec(rangeHeader);
      if (!m) throw new Error(`invalid Range header: ${rangeHeader}`);
      const start = Number(m[1]);
      const end = Number(m[2]);
      const slice = data.subarray(start, Math.min(end + 1, data.byteLength));
      const body = toArrayBuffer(slice);
      return new Response(body, {
        status: 206,
        headers: {
          "Cache-Control": "no-transform",
          "Content-Range": `bytes ${start}-${start + body.byteLength - 1}/${data.byteLength}`,
        },
      });
    }

    return new Response(toArrayBuffer(data), { status: 200, headers: { "Content-Length": String(data.byteLength) } });
  };

  return { fetch: fetcher, getCalls: () => calls };
}

describe("RemoteRangeDisk + COW overlay", () => {
  it("serves overlay writes over base reads", async () => {
    const baseBytes = new Uint8Array(new ArrayBuffer(512 * 8));
    for (let i = 0; i < baseBytes.length; i++) baseBytes[i] = (i * 7 + 3) & 0xff;

    const { fetch: fetcher, getCalls } = createRangeFetch(baseBytes);

    const caches = new Map<string, MemorySparseDisk>();
    const sparseCacheFactory: RemoteRangeDiskSparseCacheFactory = {
      async open(cacheId) {
        const hit = caches.get(cacheId);
        if (!hit) throw new Error("cache not found");
        return hit;
      },
      async create(cacheId, opts) {
        const disk = MemorySparseDisk.create({ diskSizeBytes: opts.diskSizeBytes, blockSizeBytes: opts.blockSizeBytes });
        caches.set(cacheId, disk);
        return disk;
      },
      async delete(cacheId) {
        caches.delete(cacheId);
      },
    };
    const metaMap = new Map<string, any>();
    const metadataStore: RemoteRangeDiskMetadataStore = {
      async read(cacheId) {
        return metaMap.get(cacheId) ?? null;
      },
      async write(cacheId, meta) {
        metaMap.set(cacheId, meta);
      },
      async delete(cacheId) {
        metaMap.delete(cacheId);
      },
    };

    const base = await RemoteRangeDisk.open("https://example.invalid/disk.img", {
      cacheKeyParts: { imageId: "test-img", version: "1", deliveryType: remoteRangeDeliveryType(1024) },
      chunkSize: 1024,
      fetchFn: fetcher,
      metadataStore,
      sparseCacheFactory,
      readAheadChunks: 0,
    });

    await expect(base.writeSectors(0, new Uint8Array(512))).rejects.toThrow(/read-only/);

    const overlay = MemorySparseDisk.create({ diskSizeBytes: base.capacityBytes, blockSizeBytes: 1024 });
    const cow = new OpfsCowDisk(base, overlay);

    const before = new Uint8Array(512);
    await cow.readSectors(0, before);
    expect(Array.from(before)).toEqual(Array.from(baseBytes.subarray(0, 512)));

    const patch = new Uint8Array(512);
    patch.fill(0xee);
    await cow.writeSectors(1, patch);

    const after = new Uint8Array(512);
    await cow.readSectors(1, after);
    expect(Array.from(after)).toEqual(Array.from(patch));

    const baseAfter = new Uint8Array(512);
    await base.readSectors(1, baseAfter);
    expect(Array.from(baseAfter)).toEqual(Array.from(baseBytes.subarray(512, 1024)));

    await cow.flush();
    expect(getCalls()).toBeGreaterThan(0);
  });
});
