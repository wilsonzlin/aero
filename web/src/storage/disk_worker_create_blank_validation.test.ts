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

async function sendCreateBlank(payload: any): Promise<any> {
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
      op: "create_blank",
      payload,
    },
  });

  return await response;
}

describe("disk_worker create_blank validation", () => {
  it("rejects non-raw formats", async () => {
    const resp = await sendCreateBlank({ name: "x", sizeBytes: 1024 * 1024, kind: "hdd", format: "aerospar" });
    expect(resp.ok).toBe(false);
    expect(String(resp.error?.message ?? "")).toMatch(/only raw hdd images can be created/i);
  });

  it("rejects non-sector-aligned sizes", async () => {
    const resp = await sendCreateBlank({ name: "x", sizeBytes: 123, kind: "hdd", format: "raw" });
    expect(resp.ok).toBe(false);
    expect(String(resp.error?.message ?? "")).toMatch(/multiple of 512/i);
  });
});

