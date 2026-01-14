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

describe("DiskManager message validation", () => {
  it("does not accept response messages with type inherited from Object.prototype", async () => {
    const existing = Object.getOwnPropertyDescriptor(Object.prototype, "type");
    if (existing && existing.configurable === false) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    const w = new MockWorker();
    const manager = new DiskManager({ backend: "opfs", worker: w as unknown as Worker });

    try {
      Object.defineProperty(Object.prototype, "type", { value: "response", configurable: true });

      const p = manager.listRemoteCaches();

      const result = { ok: true, caches: [], corruptKeys: [] };

      // Missing `type` must not be satisfied by prototype pollution.
      w.emit({ requestId: 1, ok: true, result });

      const raced = await Promise.race([p.then(() => "resolved", () => "rejected"), Promise.resolve("pending")]);
      expect(raced).toBe("pending");

      // A valid response should still resolve the request.
      w.emit({ type: "response", requestId: 1, ok: true, result });
      await expect(p).resolves.toEqual(result);
    } finally {
      if (existing) Object.defineProperty(Object.prototype, "type", existing);
      else Reflect.deleteProperty(Object.prototype, "type");
      manager.close();
    }
  });
});
