import { afterEach, describe, expect, it, vi } from "vitest";

import { installMemoryOpfs, MemoryDirectoryHandle } from "../test_utils/memory_opfs";
import { METADATA_VERSION, opfsWriteState, type DiskManagerState } from "./metadata";

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

  vi.clearAllMocks();
  vi.resetModules();
});

async function sendResizeDisk(format: string, newSizeBytes: unknown = 2 * 1024 * 1024): Promise<any> {
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

  const id = "disk1";
  const state: DiskManagerState = {
    version: METADATA_VERSION,
    disks: {
      [id]: {
        source: "local",
        id,
        name: "test",
        backend: "opfs",
        kind: "hdd",
        format: format as any,
        fileName: `disk1.${format}`,
        sizeBytes: 1024 * 1024,
        createdAtMs: Date.now(),
        lastUsedAtMs: undefined,
      },
    },
    mounts: {},
  };
  await opfsWriteState(state);

  await import("./disk_worker.ts");

  workerScope.onmessage?.({
    data: {
      type: "request",
      requestId,
      backend: "opfs",
      op: "resize_disk",
      payload: { id, newSizeBytes },
    },
  });

  return await response;
}

describe("disk_worker resize_disk format validation", () => {
  it("rejects resizing aerospar disks", async () => {
    const resp = await sendResizeDisk("aerospar");
    expect(resp.ok).toBe(false);
    expect(String(resp.error?.message ?? "")).toMatch(/only raw hdd images can be resized/i);
    expect(String(resp.error?.message ?? "")).toMatch(/aerospar/i);
  });

  it("rejects resizing qcow2 disks", async () => {
    const resp = await sendResizeDisk("qcow2");
    expect(resp.ok).toBe(false);
    expect(String(resp.error?.message ?? "")).toMatch(/only raw hdd images can be resized/i);
    expect(String(resp.error?.message ?? "")).toMatch(/qcow2/i);
  });

  it("rejects resizing to a non-sector-aligned size", async () => {
    const resp = await sendResizeDisk("raw", 123);
    expect(resp.ok).toBe(false);
    expect(String(resp.error?.message ?? "")).toMatch(/multiple of 512/i);
  });

  it("rejects resizing to zero bytes", async () => {
    const resp = await sendResizeDisk("raw", 0);
    expect(resp.ok).toBe(false);
    expect(String(resp.error?.message ?? "")).toMatch(/positive safe integer/i);
  });
});
