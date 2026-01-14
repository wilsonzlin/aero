import { afterEach, describe, expect, it, vi } from "vitest";

import { installMemoryOpfs, MemoryDirectoryHandle } from "../test_utils/memory_opfs";

let restoreOpfs: (() => void) | null = null;
let hadOriginalSelf = false;
let originalSelf: unknown = undefined;

afterEach(() => {
  vi.useRealTimers();

  restoreOpfs?.();
  restoreOpfs = null;

  if (!hadOriginalSelf) {
    Reflect.deleteProperty(globalThis as unknown as { self?: unknown }, "self");
  } else {
    (globalThis as unknown as { self?: unknown }).self = originalSelf;
  }
  hadOriginalSelf = false;
  originalSelf = undefined;
});

describe("disk_worker list_remote_caches", () => {
  it("lists RemoteCacheManager caches and reports corrupt keys", async () => {
    vi.resetModules();
    vi.useFakeTimers();
    const nowMs = 1_700_000_000_000;
    vi.setSystemTime(nowMs);

    const root = new MemoryDirectoryHandle("root");
    restoreOpfs = installMemoryOpfs(root).restore;

    hadOriginalSelf = Object.prototype.hasOwnProperty.call(globalThis, "self");
    originalSelf = (globalThis as unknown as { self?: unknown }).self;

    const requestId = 1;
    let resolveResponse: ((msg: any) => void) | null = null;
    const response = new Promise<any>((resolve) => {
      resolveResponse = resolve;
    });

    const workerScope: any = {
      postMessage(msg: any) {
        if (msg?.type === "response" && msg.requestId === requestId) {
          resolveResponse?.(msg);
        }
      },
    };
    (globalThis as unknown as { self?: unknown }).self = workerScope;

    await import("./disk_worker.ts");

    const { RemoteCacheManager, remoteRangeDeliveryType } = await import("./remote_cache_manager");
    const { opfsGetRemoteCacheDir } = await import("./metadata");

    const manager = await RemoteCacheManager.openOpfs();
    const opened = await manager.openCache(
      { imageId: "img", version: "v1", deliveryType: remoteRangeDeliveryType(1024) },
      { chunkSizeBytes: 1024, validators: { sizeBytes: 1024 * 1024 } },
    );
    await manager.recordCachedRange(opened.cacheKey, 0, 4096);

    // Simulate an OPFS LRU chunk cache that stores bytes in `index.json` but does not update
    // RemoteCacheManager cachedRanges (so `getCacheStatus().cachedBytes` would otherwise be 0).
    const openedLru = await manager.openCache(
      { imageId: "img2", version: "v1", deliveryType: remoteRangeDeliveryType(1024) },
      { chunkSizeBytes: 1024, validators: { sizeBytes: 1024 * 1024 } },
    );
    const indexWriteMs = nowMs + 1234;
    {
      vi.setSystemTime(indexWriteMs);
      const remoteCacheDir = await opfsGetRemoteCacheDir();
      const dir = await remoteCacheDir.getDirectoryHandle(openedLru.cacheKey, { create: false });
      const handle = await dir.getFileHandle("index.json", { create: true });
      const writable = await handle.createWritable({ keepExistingData: false });
      await writable.write(
        JSON.stringify({
          chunks: {
            "0": { byteLength: 1024, lastAccess: nowMs },
            // Simulate a partial final chunk (end-of-file), which can be smaller than chunkSize.
            "1": { byteLength: 512, lastAccess: nowMs },
          },
        }),
      );
      await writable.close();
    }

    const corruptKey = "rc1_corrupt";
    {
      const remoteCacheDir = await opfsGetRemoteCacheDir();
      const dir = await remoteCacheDir.getDirectoryHandle(corruptKey, { create: true });
      const handle = await dir.getFileHandle("meta.json", { create: true });
      const writable = await handle.createWritable({ keepExistingData: false });
      await writable.write("{ this is not valid json");
      await writable.close();
    }

    workerScope.onmessage?.({
      data: {
        type: "request",
        requestId,
        backend: "opfs",
        op: "list_remote_caches",
        payload: {},
      },
    });

    const resp = await response;
    expect(resp.ok).toBe(true);
    expect(resp.result).toMatchObject({
      ok: true,
      corruptKeys: [corruptKey],
    });

    const list = resp.result as { caches: any[]; corruptKeys: string[] };
    expect(list.caches).toHaveLength(2);

    // Results should be deterministically ordered by `lastAccessedAtMs` desc then `cacheKey`.
    expect(list.caches.map((c) => c.cacheKey)).toEqual([openedLru.cacheKey, opened.cacheKey]);

    const fromRanges = list.caches.find((c) => c.cacheKey === opened.cacheKey);
    expect(fromRanges).toMatchObject({
      cacheKey: opened.cacheKey,
      cachedBytes: 4096,
      lastAccessedAtMs: nowMs,
    });

    const fromIndex = list.caches.find((c) => c.cacheKey === openedLru.cacheKey);
    expect(fromIndex).toMatchObject({
      cacheKey: openedLru.cacheKey,
      cachedBytes: 1536,
      lastAccessedAtMs: indexWriteMs,
      cachedChunks: 2,
    });
  });

  it("sorts caches by lastAccessedAtMs desc, then cacheKey", async () => {
    vi.resetModules();
    vi.useFakeTimers();
    const nowMs = 1_700_000_000_000;
    vi.setSystemTime(nowMs);

    const root = new MemoryDirectoryHandle("root");
    restoreOpfs = installMemoryOpfs(root).restore;

    hadOriginalSelf = Object.prototype.hasOwnProperty.call(globalThis, "self");
    originalSelf = (globalThis as unknown as { self?: unknown }).self;

    const requestId = 1;
    let resolveResponse: ((msg: any) => void) | null = null;
    const response = new Promise<any>((resolve) => {
      resolveResponse = resolve;
    });

    const workerScope: any = {
      postMessage(msg: any) {
        if (msg?.type === "response" && msg.requestId === requestId) {
          resolveResponse?.(msg);
        }
      },
    };
    (globalThis as unknown as { self?: unknown }).self = workerScope;

    await import("./disk_worker.ts");

    const { RemoteCacheManager, remoteRangeDeliveryType } = await import("./remote_cache_manager");

    const manager = await RemoteCacheManager.openOpfs();
    const a = await manager.openCache(
      { imageId: "a", version: "v1", deliveryType: remoteRangeDeliveryType(1024) },
      { chunkSizeBytes: 1024, validators: { sizeBytes: 1024 * 1024 } },
    );
    const b = await manager.openCache(
      { imageId: "b", version: "v1", deliveryType: remoteRangeDeliveryType(1024) },
      { chunkSizeBytes: 1024, validators: { sizeBytes: 1024 * 1024 } },
    );
    const c = await manager.openCache(
      { imageId: "c", version: "v1", deliveryType: remoteRangeDeliveryType(1024) },
      { chunkSizeBytes: 1024, validators: { sizeBytes: 1024 * 1024 } },
    );

    workerScope.onmessage?.({
      data: {
        type: "request",
        requestId,
        backend: "opfs",
        op: "list_remote_caches",
        payload: {},
      },
    });

    const resp = await response;
    expect(resp.ok).toBe(true);
    expect(resp.result).toMatchObject({ ok: true, corruptKeys: [] });

    const list = resp.result as { caches: any[] };
    const expectedKeys = [a.cacheKey, b.cacheKey, c.cacheKey].sort((x, y) => x.localeCompare(y));
    expect(list.caches.map((x) => x.cacheKey)).toEqual(expectedKeys);
  });

  it("returns an empty list for non-OPFS backends", async () => {
    vi.resetModules();

    const requestId = 1;
    let resolveResponse: ((msg: any) => void) | null = null;
    const response = new Promise<any>((resolve) => {
      resolveResponse = resolve;
    });

    hadOriginalSelf = Object.prototype.hasOwnProperty.call(globalThis, "self");
    originalSelf = (globalThis as unknown as { self?: unknown }).self;

    const workerScope: any = {
      postMessage(msg: any) {
        if (msg?.type === "response" && msg.requestId === requestId) {
          resolveResponse?.(msg);
        }
      },
    };
    (globalThis as unknown as { self?: unknown }).self = workerScope;

    await import("./disk_worker.ts");

    workerScope.onmessage?.({
      data: {
        type: "request",
        requestId,
        backend: "idb",
        op: "list_remote_caches",
        payload: {},
      },
    });

    const resp = await response;
    expect(resp.ok).toBe(true);
    expect(resp.result).toEqual({ ok: true, caches: [], corruptKeys: [] });
  });
});
