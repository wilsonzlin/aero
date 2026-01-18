import { afterEach, describe, expect, it, vi } from "vitest";

import { installMemoryOpfs, MemoryDirectoryHandle } from "../test_utils/memory_opfs";

function toArrayBufferUint8(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  // Newer TS libdefs model typed arrays as `Uint8Array<ArrayBufferLike>`, but `fetch`/`Response`
  // bodies are typed to accept only ArrayBuffer-backed views.
  return bytes.buffer instanceof ArrayBuffer ? (bytes as unknown as Uint8Array<ArrayBuffer>) : new Uint8Array(bytes);
}

let probeSize = 1024 * 512;

const originalFetchDescriptor = Object.getOwnPropertyDescriptor(globalThis, "fetch");
function stubFetch(value: unknown): void {
  Object.defineProperty(globalThis, "fetch", { value, configurable: true, enumerable: true, writable: true });
}
function restoreFetch(): void {
  if (originalFetchDescriptor) {
    Object.defineProperty(globalThis, "fetch", originalFetchDescriptor);
  } else {
    Reflect.deleteProperty(globalThis as unknown as { fetch?: unknown }, "fetch");
  }
}

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
  restoreFetch();

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
  it("rejects non-string url without calling toString()", async () => {
    const hostile = {
      toString() {
        throw new Error("boom");
      },
    };
    const resp = await sendAddRemote({ url: hostile });
    expect(resp.ok).toBe(false);
    expect(resp.error?.message).toMatch(/Missing url/i);
  });

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

  it("rejects remote qcow2 images by content sniffing", async () => {
    const bytesForRange = (start: number, len: number): Uint8Array<ArrayBuffer> => {
      const out = new Uint8Array(len);
      if (start === 0 && len >= 8) {
        // qcow2 magic + version 3 (big-endian)
        out.set([0x51, 0x46, 0x49, 0xfb, 0x00, 0x00, 0x00, 0x03], 0);
      }
      return out;
    };

    stubFetch(async (_input: any, init?: any) => {
      const rangeHeader = init?.headers?.Range ?? init?.headers?.range ?? "";
      const m = /bytes=(\d+)-(\d+)/.exec(String(rangeHeader));
      const start = m ? Number(m[1]) : 0;
      const end = m ? Number(m[2]) : 0;
      const len = end - start + 1;
      const body = bytesForRange(start, len);
      return new Response(toArrayBufferUint8(body), {
        status: 206,
        headers: {
          "cache-control": "no-transform",
          "content-length": String(body.byteLength),
          "content-range": `bytes ${start}-${end}/${probeSize}`,
        },
      });
    });

    const resp = await sendAddRemote({ url: "https://example.invalid/disk.img" });
    expect(resp.ok).toBe(false);
    expect(String(resp.error?.message ?? "")).toMatch(/qcow2/i);
  });

  it("adds raw remote disks when bytes do not look like a container", async () => {
    stubFetch(async (_input: any, init?: any) => {
      const rangeHeader = init?.headers?.Range ?? init?.headers?.range ?? "";
      const m = /bytes=(\d+)-(\d+)/.exec(String(rangeHeader));
      const start = m ? Number(m[1]) : 0;
      const end = m ? Number(m[2]) : 0;
      const len = end - start + 1;
      const body = new Uint8Array(len);
      return new Response(toArrayBufferUint8(body), {
        status: 206,
        headers: {
          "cache-control": "no-transform",
          "content-length": String(body.byteLength),
          "content-range": `bytes ${start}-${end}/${probeSize}`,
        },
      });
    });

    const resp = await sendAddRemote({ url: "https://example.invalid/disk.img" });
    expect(resp.ok).toBe(true);
    expect(resp.result?.format).toBe("raw");
    expect(resp.result?.kind).toBe("hdd");
  });

  it("auto-detects ISO9660 for remote .img URLs", async () => {
    const ISO_PVD_SIG_OFFSET = 0x8001;

    stubFetch(async (_input: any, init?: any) => {
      const rangeHeader = init?.headers?.Range ?? init?.headers?.range ?? "";
      const m = /bytes=(\d+)-(\d+)/.exec(String(rangeHeader));
      const start = m ? Number(m[1]) : 0;
      const end = m ? Number(m[2]) : 0;
      const len = end - start + 1;
      const body = new Uint8Array(len);
      if (start === ISO_PVD_SIG_OFFSET && len === 5) {
        body.set([0x43, 0x44, 0x30, 0x30, 0x31], 0); // "CD001"
      }
      return new Response(toArrayBufferUint8(body), {
        status: 206,
        headers: {
          "cache-control": "no-transform",
          "content-length": String(body.byteLength),
          "content-range": `bytes ${start}-${end}/${probeSize}`,
        },
      });
    });

    const resp = await sendAddRemote({ url: "https://example.invalid/disk.img" });
    expect(resp.ok).toBe(true);
    expect(resp.result?.format).toBe("iso");
    expect(resp.result?.kind).toBe("cd");
  });
});
