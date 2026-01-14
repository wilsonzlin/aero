import { afterEach, describe, expect, it, vi } from "vitest";

import { installMemoryOpfs, MemoryDirectoryHandle } from "../test_utils/memory_opfs";

let probeSize = 1024 * 512;

vi.mock("../platform/remote_disk", () => ({
  probeRemoteDisk: async () => ({
    size: probeSize,
    etag: null,
    lastModified: null,
    acceptRanges: "bytes",
    rangeProbeStatus: 206,
    partialOk: true,
    contentRange: `bytes 0-0/${probeSize}`,
  }),
  stableCacheKey: async () => "cache-key",
}));

let restoreOpfs: (() => void) | null = null;
let hadOriginalSelf = false;
let originalSelf: unknown = undefined;

afterEach(() => {
  restoreOpfs?.();
  restoreOpfs = null;
  probeSize = 1024 * 512;

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

async function sendAddRemote(payload: any): Promise<any> {
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
      op: "add_remote",
      payload,
    },
  });

  return await response;
}

describe("disk_worker add_remote validation", () => {
  it("rejects blockSizeBytes > 64MiB", async () => {
    const resp = await sendAddRemote({
      url: "https://example.invalid/disk.img",
      blockSizeBytes: 64 * 1024 * 1024 + 512,
    });
    expect(resp.ok).toBe(false);
    expect(resp.error?.message).toMatch(/blockSizeBytes/i);
    expect(resp.error?.message).toMatch(/64/i);
  });

  it("rejects prefetchSequentialBlocks > 1024", async () => {
    const resp = await sendAddRemote({
      url: "https://example.invalid/disk.img",
      prefetchSequentialBlocks: 1025,
    });
    expect(resp.ok).toBe(false);
    expect(resp.error?.message).toMatch(/prefetchSequentialBlocks/i);
    expect(resp.error?.message).toMatch(/1024/);
  });

  it("rejects prefetchSequentialBlocks that exceed 512MiB total prefetch bytes", async () => {
    const resp = await sendAddRemote({
      url: "https://example.invalid/disk.img",
      // Default block size is 1 MiB; 513 * 1 MiB > 512 MiB.
      prefetchSequentialBlocks: 513,
    });
    expect(resp.ok).toBe(false);
    expect(resp.error?.message).toMatch(/prefetchSequentialBlocks/i);
    expect(resp.error?.message).toMatch(/512/i);
  });

  it("rejects negative cacheLimitBytes", async () => {
    const resp = await sendAddRemote({
      url: "https://example.invalid/disk.img",
      cacheLimitBytes: -1,
    });
    expect(resp.ok).toBe(false);
    expect(resp.error?.message).toMatch(/cacheLimitBytes/i);
    expect(resp.error?.message).toMatch(/non-negative/i);
  });

  it("rejects remote sizes larger than MAX_SAFE_INTEGER", async () => {
    probeSize = 9007199254740992; // 2^53 (not a safe JS integer)
    const resp = await sendAddRemote({
      url: "https://example.invalid/disk.img",
    });
    expect(resp.ok).toBe(false);
    expect(resp.error?.message).toMatch(/safe integer/i);
  });
});
