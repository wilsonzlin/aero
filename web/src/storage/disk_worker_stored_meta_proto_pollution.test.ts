import { afterEach, describe, expect, it, vi } from "vitest";

import { opfsGetDiskSizeBytes } from "./import_export";
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

  vi.clearAllMocks();
  vi.resetModules();
});

async function setupWorkerHarness(): Promise<{ send: (data: any) => Promise<any> }> {
  vi.resetModules();

  const root = new MemoryDirectoryHandle("root");
  restoreOpfs = installMemoryOpfs(root).restore;

  hadOriginalSelf = Object.prototype.hasOwnProperty.call(globalThis, "self");
  originalSelf = (globalThis as unknown as { self?: unknown }).self;

  const pending = new Map<number, (msg: any) => void>();
  const workerScope: any = {
    postMessage(msg: any) {
      if (msg?.type === "response" && typeof msg.requestId === "number") {
        pending.get(msg.requestId)?.(msg);
      }
    },
  };
  (globalThis as unknown as { self?: unknown }).self = workerScope;

  await import("./disk_worker.ts");

  const send = async (data: any): Promise<any> => {
    const requestId = data?.requestId ?? 1;
    const response = new Promise<any>((resolve) => {
      pending.set(requestId, resolve);
    });
    workerScope.onmessage?.({ data });
    return await response;
  };

  return { send };
}

describe("disk_worker stored metadata prototype pollution hardening", () => {
  it("stat_disk/delete_disk do not observe inherited Object.prototype.remote", async () => {
    const remoteExisting = Object.getOwnPropertyDescriptor(Object.prototype, "remote");
    if (remoteExisting && remoteExisting.configurable === false) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    const { send } = await setupWorkerHarness();

    const created = await send({
      type: "request",
      requestId: 1,
      backend: "opfs",
      op: "import_file",
      payload: { file: new File([new Uint8Array(512)], "disk.img") },
    });
    expect(created.ok).toBe(true);
    const meta = created.result;
    expect(typeof meta?.id).toBe("string");
    expect(typeof meta?.fileName).toBe("string");
    expect(meta?.sizeBytes).toBe(512);

    const diskId = meta.id as string;
    const fileName = meta.fileName as string;

    try {
      Object.defineProperty(Object.prototype, "remote", {
        value: { url: "https://example.com/evil.img" },
        configurable: true,
        writable: true,
      });

      const stat = await send({
        type: "request",
        requestId: 2,
        backend: "opfs",
        op: "stat_disk",
        payload: { id: diskId },
      });
      expect(stat.ok).toBe(true);
      expect(stat.result?.actualSizeBytes).toBe(512);

      const del = await send({
        type: "request",
        requestId: 3,
        backend: "opfs",
        op: "delete_disk",
        payload: { id: diskId },
      });
      expect(del.ok).toBe(true);

      await expect(opfsGetDiskSizeBytes(fileName)).rejects.toBeTruthy();
    } finally {
      if (remoteExisting) Object.defineProperty(Object.prototype, "remote", remoteExisting);
      else Reflect.deleteProperty(Object.prototype, "remote");
    }
  });
});
