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

async function writeValidMeta(dir: FileSystemDirectoryHandle, lastAccessedAtMs: number): Promise<void> {
  const meta = {
    version: 1,
    imageId: "img",
    imageVersion: "v1",
    deliveryType: "range:1024",
    validators: { sizeBytes: 1024 * 1024 },
    chunkSizeBytes: 1024,
    createdAtMs: lastAccessedAtMs,
    lastAccessedAtMs,
    cachedRanges: [],
  };

  const handle = await dir.getFileHandle("meta.json", { create: true });
  const writable = await handle.createWritable({ keepExistingData: false });
  await writable.write(JSON.stringify(meta, null, 2));
  await writable.close();
}

describe("disk_worker prune_remote_caches", () => {
  it("rejects non-number olderThanMs without calling valueOf()", async () => {
    vi.resetModules();

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

    const hostile = {
      valueOf() {
        throw new Error("boom");
      },
    };

    workerScope.onmessage?.({
      data: {
        type: "request",
        requestId,
        backend: "opfs",
        op: "prune_remote_caches",
        payload: { olderThanMs: hostile },
      },
    });

    const resp = await response;
    expect(resp.ok).toBe(false);
    expect(String(resp.error?.message ?? "")).toMatch(/olderThanMs/i);
  });

  it("prunes stale OPFS RemoteCacheManager caches by lastAccessedAtMs (and corrupt meta)", async () => {
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

    const { opfsGetRemoteCacheDir } = await import("./metadata");
    const remoteCacheDir = await opfsGetRemoteCacheDir();

    const freshKey = "rc1_fresh";
    const staleKey1 = "rc1_stale1";
    const staleKey2 = "rc1_stale2";
    const corruptKey = "rc1_corrupt";

    await writeValidMeta(await remoteCacheDir.getDirectoryHandle(freshKey, { create: true }), nowMs - 1000);
    await writeValidMeta(await remoteCacheDir.getDirectoryHandle(staleKey1, { create: true }), nowMs - 5000);
    await writeValidMeta(await remoteCacheDir.getDirectoryHandle(staleKey2, { create: true }), nowMs - 10_000);

    // Corrupt meta => eligible for pruning.
    {
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
        op: "prune_remote_caches",
        payload: { olderThanMs: 4000 },
      },
    });

    const resp = await response;
    expect(resp.ok).toBe(true);
    expect(resp.result).toMatchObject({ ok: true, examined: 4, pruned: 3 });

    // Fresh cache should remain.
    await expect(remoteCacheDir.getDirectoryHandle(freshKey, { create: false })).resolves.toBeTruthy();

    // Stale / corrupt caches should be removed.
    for (const key of [staleKey1, staleKey2, corruptKey]) {
      await expect(remoteCacheDir.getDirectoryHandle(key, { create: false })).rejects.toMatchObject({ name: "NotFoundError" });
    }
  });

  it("uses index.json lastModified as a best-effort last-access signal (LRU chunk caches)", async () => {
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

    const { opfsGetRemoteCacheDir } = await import("./metadata");
    const remoteCacheDir = await opfsGetRemoteCacheDir();

    const staleKey = "rc1_stale";
    const lruKey = "rc1_lru";

    // Both caches have old meta timestamps (stale), but the "lru" cache has an index.json written "now".
    await writeValidMeta(await remoteCacheDir.getDirectoryHandle(staleKey, { create: true }), nowMs - 10_000);
    const lruDir = await remoteCacheDir.getDirectoryHandle(lruKey, { create: true });
    await writeValidMeta(lruDir, nowMs - 10_000);

    // index.json lastModified should be treated as a last-access indicator for LRU caches.
    {
      const handle = await lruDir.getFileHandle("index.json", { create: true });
      const writable = await handle.createWritable({ keepExistingData: false });
      await writable.write("{}");
      await writable.close();
    }

    workerScope.onmessage?.({
      data: {
        type: "request",
        requestId,
        backend: "opfs",
        op: "prune_remote_caches",
        payload: { olderThanMs: 4000 },
      },
    });

    const resp = await response;
    expect(resp.ok).toBe(true);
    expect(resp.result).toMatchObject({ ok: true, examined: 2, pruned: 1 });

    // staleKey should be pruned due to old meta and no index.json activity.
    await expect(remoteCacheDir.getDirectoryHandle(staleKey, { create: false })).rejects.toMatchObject({ name: "NotFoundError" });

    // lruKey should remain because index.json was recently written.
    await expect(remoteCacheDir.getDirectoryHandle(lruKey, { create: false })).resolves.toBeTruthy();
  });

  it("supports dryRun (reports keys without deleting)", async () => {
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

    const { opfsGetRemoteCacheDir } = await import("./metadata");
    const remoteCacheDir = await opfsGetRemoteCacheDir();

    const key1 = "rc1_keep";
    const key2 = "rc1_prune";
    await writeValidMeta(await remoteCacheDir.getDirectoryHandle(key1, { create: true }), nowMs - 1000);
    await writeValidMeta(await remoteCacheDir.getDirectoryHandle(key2, { create: true }), nowMs - 10_000);

    workerScope.onmessage?.({
      data: {
        type: "request",
        requestId,
        backend: "opfs",
        op: "prune_remote_caches",
        payload: { olderThanMs: 4000, dryRun: true },
      },
    });

    const resp = await response;
    expect(resp.ok).toBe(true);
    expect(resp.result).toMatchObject({ ok: true, examined: 2, pruned: 1, prunedKeys: [key2] });

    // dryRun must not delete anything.
    await expect(remoteCacheDir.getDirectoryHandle(key1, { create: false })).resolves.toBeTruthy();
    await expect(remoteCacheDir.getDirectoryHandle(key2, { create: false })).resolves.toBeTruthy();
  });

  it("prunes to maxCaches keeping the most-recent caches", async () => {
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

    const { opfsGetRemoteCacheDir } = await import("./metadata");
    const remoteCacheDir = await opfsGetRemoteCacheDir();

    const keys = ["rc1_1", "rc1_2", "rc1_3", "rc1_4"];
    // Descending recency: rc1_1 most recent, rc1_4 oldest.
    await writeValidMeta(await remoteCacheDir.getDirectoryHandle(keys[0]!, { create: true }), nowMs - 1000);
    await writeValidMeta(await remoteCacheDir.getDirectoryHandle(keys[1]!, { create: true }), nowMs - 2000);
    await writeValidMeta(await remoteCacheDir.getDirectoryHandle(keys[2]!, { create: true }), nowMs - 3000);
    await writeValidMeta(await remoteCacheDir.getDirectoryHandle(keys[3]!, { create: true }), nowMs - 4000);

    workerScope.onmessage?.({
      data: {
        type: "request",
        requestId,
        backend: "opfs",
        op: "prune_remote_caches",
        payload: { olderThanMs: 999_999_999, maxCaches: 2 },
      },
    });

    const resp = await response;
    expect(resp.ok).toBe(true);
    expect(resp.result).toMatchObject({ ok: true, examined: 4, pruned: 2 });

    // Keep 2 most recent.
    await expect(remoteCacheDir.getDirectoryHandle(keys[0]!, { create: false })).resolves.toBeTruthy();
    await expect(remoteCacheDir.getDirectoryHandle(keys[1]!, { create: false })).resolves.toBeTruthy();

    // Prune 2 oldest.
    await expect(remoteCacheDir.getDirectoryHandle(keys[2]!, { create: false })).rejects.toMatchObject({ name: "NotFoundError" });
    await expect(remoteCacheDir.getDirectoryHandle(keys[3]!, { create: false })).rejects.toMatchObject({ name: "NotFoundError" });
  });
});
