import { describe, expect, it } from "vitest";

import { DiskManager } from "./disk_manager";

class MockWorker {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  onmessage: ((event: any) => void) | null = null;
  lastMessage: any;

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  postMessage(msg: any): void {
    this.lastMessage = msg;
  }

  terminate(): void {
    // no-op
  }

  emit(data: unknown): void {
    this.onmessage?.({ data });
  }
}

describe("DiskManager request port prototype pollution hardening", () => {
  it("does not observe inherited Object.prototype.port when sending requests", async () => {
    const existing = Object.getOwnPropertyDescriptor(Object.prototype, "port");
    if (existing && existing.configurable === false) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    const w = new MockWorker();
    const manager = new DiskManager({ backend: "opfs", worker: w as unknown as Worker });

    try {
      Object.defineProperty(Object.prototype, "port", { value: "evil", configurable: true, writable: true });

      const p = manager.listRemoteCaches();

      // Requests that do not explicitly carry a MessagePort must not pick one up from prototype
      // pollution.
      expect(w.lastMessage?.port).toBeUndefined();

      const requestId = w.lastMessage?.requestId ?? 1;
      const result = { ok: true, caches: [], corruptKeys: [] };
      w.emit({ type: "response", requestId, ok: true, result });
      await expect(p).resolves.toEqual(result);
    } finally {
      if (existing) Object.defineProperty(Object.prototype, "port", existing);
      else delete (Object.prototype as any).port;
      manager.close();
    }
  });
});

