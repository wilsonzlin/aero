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

  vi.clearAllMocks();
  vi.resetModules();
});

async function sendSetMounts(payload: any): Promise<any> {
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
      op: "set_mounts",
      payload,
    },
  });

  return await response;
}

describe("disk_worker set_mounts validation", () => {
  it("ignores mount IDs inherited from Object.prototype", async () => {
    const hddExisting = Object.getOwnPropertyDescriptor(Object.prototype, "hddId");
    const cdExisting = Object.getOwnPropertyDescriptor(Object.prototype, "cdId");
    if ((hddExisting && hddExisting.configurable === false) || (cdExisting && cdExisting.configurable === false)) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    try {
      Object.defineProperty(Object.prototype, "hddId", { value: "evil", configurable: true });
      Object.defineProperty(Object.prototype, "cdId", { value: "evil2", configurable: true });

      const resp = await sendSetMounts({});
      expect(resp.ok).toBe(true);
      // The worker should return a sanitized mounts object with no inherited IDs.
      expect({ ...(resp.result ?? {}) }).toEqual({});
    } finally {
      if (hddExisting) Object.defineProperty(Object.prototype, "hddId", hddExisting);
      else delete (Object.prototype as any).hddId;
      if (cdExisting) Object.defineProperty(Object.prototype, "cdId", cdExisting);
      else delete (Object.prototype as any).cdId;
    }
  });
});

