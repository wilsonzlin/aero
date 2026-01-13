import { afterEach, describe, expect, it, vi } from "vitest";

import { installMemoryOpfs, MemoryDirectoryHandle } from "../test_utils/memory_opfs";

let restoreOpfs: (() => void) | null = null;
let hadOriginalSelf = false;
let originalSelf: unknown = undefined;

afterEach(() => {
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

describe("disk_worker delete_disk", () => {
  it("deletes the RemoteRangeDisk per-disk .remote-range-meta.json sidecar (OPFS)", async () => {
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

    const posted: any[] = [];
    const workerScope: any = {
      postMessage(msg: any) {
        posted.push(msg);
        if (msg?.type === "response" && msg.requestId === requestId) {
          resolveResponse?.(msg);
        }
      },
    };
    (globalThis as unknown as { self?: unknown }).self = workerScope;

    await import("./disk_worker.ts");

    const { createMetadataStore, opfsGetDisksDir } = await import("./metadata");
    const store = createMetadataStore("opfs");

    const meta = {
      source: "remote",
      id: "remote-range-test",
      name: "Remote Range Test",
      kind: "cd",
      format: "iso",
      sizeBytes: 1024 * 1024,
      createdAtMs: Date.now(),
      lastUsedAtMs: undefined,
      remote: {
        imageId: "remote-range-test",
        version: "v1",
        delivery: "range",
        urls: { url: "https://example.invalid/disk.iso" },
      },
      cache: {
        chunkSizeBytes: 1024,
        backend: "opfs",
        fileName: "remote-range-test.cache.aerospar",
        overlayFileName: "remote-range-test.overlay.aerospar",
        overlayBlockSizeBytes: 1024,
      },
    } as const;

    await store.putDisk(meta as any);

    const disksDir = await opfsGetDisksDir();

    // Create fake cache + sidecar metadata that should be removed by delete_disk.
    const cacheHandle = await disksDir.getFileHandle(meta.cache.fileName, { create: true });
    const cacheWritable = await cacheHandle.createWritable({ keepExistingData: false });
    await cacheWritable.write(new Uint8Array([1, 2, 3]));
    await cacheWritable.close();

    const sidecarName = `${meta.cache.fileName}.remote-range-meta.json`;
    const sidecarHandle = await disksDir.getFileHandle(sidecarName, { create: true });
    const sidecarWritable = await sidecarHandle.createWritable({ keepExistingData: false });
    await sidecarWritable.write(JSON.stringify({ cachedRanges: [] }, null, 2));
    await sidecarWritable.close();

    // Sanity: files exist pre-delete.
    expect((await cacheHandle.getFile()).size).toBe(3);
    expect((await sidecarHandle.getFile()).size).toBeGreaterThan(0);

    workerScope.onmessage?.({
      data: {
        type: "request",
        requestId,
        backend: "opfs",
        op: "delete_disk",
        payload: { id: meta.id },
      },
    });

    const resp = await response;
    expect(resp.ok).toBe(true);

    // Both the sparse cache file and the range-meta sidecar must be deleted.
    await expect(disksDir.getFileHandle(meta.cache.fileName, { create: false })).rejects.toMatchObject({
      name: "NotFoundError",
    });
    await expect(disksDir.getFileHandle(sidecarName, { create: false })).rejects.toMatchObject({ name: "NotFoundError" });
  });
});
