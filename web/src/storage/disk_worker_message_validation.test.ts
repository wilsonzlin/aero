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

async function sendRawMessage(data: any): Promise<any> {
  vi.resetModules();

  const root = new MemoryDirectoryHandle("root");
  restoreOpfs = installMemoryOpfs(root).restore;

  hadOriginalSelf = Object.prototype.hasOwnProperty.call(globalThis, "self");
  originalSelf = (globalThis as unknown as { self?: unknown }).self;

  const requestId = data?.requestId ?? 1;
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

  workerScope.onmessage?.({ data });

  return await response;
}

describe("disk_worker message validation", () => {
  it("does not accept top-level fields inherited from Object.prototype", async () => {
    const backendExisting = Object.getOwnPropertyDescriptor(Object.prototype, "backend");
    const opExisting = Object.getOwnPropertyDescriptor(Object.prototype, "op");
    if ((backendExisting && backendExisting.configurable === false) || (opExisting && opExisting.configurable === false)) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    try {
      Object.defineProperty(Object.prototype, "backend", { value: "opfs", configurable: true });
      Object.defineProperty(Object.prototype, "op", { value: "list_disks", configurable: true });

      const resp = await sendRawMessage({ type: "request", requestId: 1 });
      expect(resp.ok).toBe(false);
      expect(String(resp.error?.message ?? "")).toMatch(/backend/i);
    } finally {
      if (backendExisting) Object.defineProperty(Object.prototype, "backend", backendExisting);
      else delete (Object.prototype as any).backend;
      if (opExisting) Object.defineProperty(Object.prototype, "op", opExisting);
      else delete (Object.prototype as any).op;
    }
  });
});

