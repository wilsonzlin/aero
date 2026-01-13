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

describe("disk_worker remote validation", () => {
  it("rejects OPFS create_remote chunkSizeBytes larger than 64MiB", async () => {
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

    workerScope.onmessage?.({
      data: {
        type: "request",
        requestId,
        backend: "opfs",
        op: "create_remote",
        payload: {
          name: "Remote disk",
          imageId: "remote-validation",
          version: "v1",
          delivery: "range",
          sizeBytes: 1024 * 1024,
          kind: "hdd",
          format: "raw",
          urls: { url: "https://example.invalid/disk.img" },
          chunkSizeBytes: 128 * 1024 * 1024,
        },
      },
    });

    const resp = await response;
    expect(resp.ok).toBe(false);
    expect(resp.error?.message ?? "").toMatch(/chunkSizeBytes/i);
  });

  it("rejects OPFS update_remote overlayBlockSizeBytes larger than 64MiB", async () => {
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

    const { createMetadataStore } = await import("./metadata");
    const store = createMetadataStore("opfs");

    const meta = {
      source: "remote",
      id: "remote-validation-test",
      name: "Remote Validation Test",
      kind: "hdd",
      format: "raw",
      sizeBytes: 1024 * 1024,
      createdAtMs: Date.now(),
      lastUsedAtMs: undefined,
      remote: {
        imageId: "remote-validation-test",
        version: "v1",
        delivery: "range",
        urls: { url: "https://example.invalid/disk.img" },
      },
      cache: {
        chunkSizeBytes: 1024 * 1024,
        backend: "opfs",
        fileName: "remote-validation-test.cache.aerospar",
        overlayFileName: "remote-validation-test.overlay.aerospar",
        overlayBlockSizeBytes: 1024 * 1024,
      },
    } as const;

    await store.putDisk(meta as any);

    workerScope.onmessage?.({
      data: {
        type: "request",
        requestId,
        backend: "opfs",
        op: "update_remote",
        payload: {
          id: meta.id,
          overlayBlockSizeBytes: 128 * 1024 * 1024,
        },
      },
    });

    const resp = await response;
    expect(resp.ok).toBe(false);
    expect(resp.error?.message ?? "").toMatch(/overlayBlockSizeBytes/i);
  });
});

