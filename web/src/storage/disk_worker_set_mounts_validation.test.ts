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

function toArrayBufferUint8(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  return bytes.buffer instanceof ArrayBuffer ? (bytes as unknown as Uint8Array<ArrayBuffer>) : new Uint8Array(bytes);
}

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

async function setupWorkerHarness(): Promise<{ send: (req: { requestId: number; op: string; payload: any }) => Promise<any> }> {
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

  const send = async (req: { requestId: number; op: string; payload: any }): Promise<any> => {
    const response = new Promise<any>((resolve) => pending.set(req.requestId, resolve));
    workerScope.onmessage?.({
      data: {
        type: "request",
        requestId: req.requestId,
        backend: "opfs",
        op: req.op,
        payload: req.payload,
      },
    });
    return await response;
  };

  return { send };
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
  }, 20_000);

  it("rejects mounting qcow2/vhd images as HDDs", async () => {
    const { send } = await setupWorkerHarness();

    const qcow2 = new Uint8Array(72);
    qcow2.set([0x51, 0x46, 0x49, 0xfb], 0); // "QFI\xfb"
    new DataView(qcow2.buffer).setUint32(4, 3, false);
    const file = new File([toArrayBufferUint8(qcow2)], "disk.img");

    const imported = await send({ requestId: 1, op: "import_file", payload: { file } });
    expect(imported.ok).toBe(true);
    const id = imported.result?.id;
    expect(typeof id).toBe("string");

    const resp = await send({ requestId: 2, op: "set_mounts", payload: { hddId: id } });
    expect(resp.ok).toBe(false);
    expect(String(resp.error?.message ?? "")).toMatch(/qcow2|vhd/i);
  }, 20_000);
});
