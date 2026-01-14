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
      else Reflect.deleteProperty(Object.prototype, "backend");
      if (opExisting) Object.defineProperty(Object.prototype, "op", opExisting);
      else Reflect.deleteProperty(Object.prototype, "op");
    }
  });

  it("does not accept payload fields inherited from Object.prototype", async () => {
    const fields = ["name", "imageId", "version", "delivery", "sizeBytes", "urls"] as const;
    const existing = Object.fromEntries(fields.map((k) => [k, Object.getOwnPropertyDescriptor(Object.prototype, k)])) as Record<
      (typeof fields)[number],
      PropertyDescriptor | undefined
    >;
    if (Object.values(existing).some((d) => d && d.configurable === false)) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    try {
      // Set fields as writable so we do not interfere with unrelated code that assigns e.g. `.name`
      // on its own objects (like our in-memory OPFS test doubles).
      Object.defineProperty(Object.prototype, "name", { value: "polluted", configurable: true, writable: true });
      Object.defineProperty(Object.prototype, "imageId", { value: "img", configurable: true, writable: true });
      Object.defineProperty(Object.prototype, "version", { value: "v1", configurable: true, writable: true });
      Object.defineProperty(Object.prototype, "delivery", { value: "range", configurable: true, writable: true });
      Object.defineProperty(Object.prototype, "sizeBytes", { value: 512, configurable: true, writable: true });
      Object.defineProperty(Object.prototype, "urls", {
        value: { url: "https://example.com/disk.img" },
        configurable: true,
        writable: true,
      });

      const resp = await sendRawMessage({ type: "request", requestId: 1, backend: "opfs", op: "create_remote", payload: {} });
      expect(resp.ok).toBe(false);
      expect(String(resp.error?.message ?? "")).toMatch(/name/i);
    } finally {
      for (const k of fields) {
        const desc = existing[k];
        if (desc) Object.defineProperty(Object.prototype, k, desc);
        else Reflect.deleteProperty(Object.prototype, k);
      }
    }
  });

  it("update_remote ignores inherited patch fields", async () => {
    const { send } = await setupWorkerHarness();

    const create = await send({
      type: "request",
      requestId: 1,
      backend: "opfs",
      op: "create_remote",
      payload: {
        name: "original",
        imageId: "img",
        version: "v1",
        delivery: "range",
        sizeBytes: 512,
        urls: { url: "https://example.com/disk.img" },
      },
    });
    expect(create.ok).toBe(true);
    const id = create.result?.id;
    expect(typeof id).toBe("string");

    const nameExisting = Object.getOwnPropertyDescriptor(Object.prototype, "name");
    if (nameExisting && nameExisting.configurable === false) return;

    try {
      Object.defineProperty(Object.prototype, "name", { value: "polluted", configurable: true, writable: true });

      const update = await send({ type: "request", requestId: 2, backend: "opfs", op: "update_remote", payload: { id } });
      expect(update.ok).toBe(true);
      expect(update.result?.name).toBe("original");
    } finally {
      if (nameExisting) Object.defineProperty(Object.prototype, "name", nameExisting);
      else Reflect.deleteProperty(Object.prototype, "name");
    }
  });
});
