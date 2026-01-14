import { describe, expect, it } from "vitest";

import { DiskManager } from "./disk_manager";
import type { DiskImageMetadata } from "./metadata";

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

describe("DiskManager.exportDiskStream message validation", () => {
  it("does not accept port messages with type inherited from Object.prototype", async () => {
    const existing = Object.getOwnPropertyDescriptor(Object.prototype, "type");
    if (existing && existing.configurable === false) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    const w = new MockWorker();
    const manager = new DiskManager({ backend: "opfs", worker: w as unknown as Worker });

    const exportPromise = manager.exportDiskStream("disk1");
    const requestId = w.lastMessage?.requestId ?? 1;
    const port = w.lastMessage?.port as MessagePort | undefined;
    if (!port) throw new Error("expected exportDiskStream to transfer a MessagePort");

    const meta: DiskImageMetadata = {
      source: "local",
      id: "disk1",
      name: "disk1",
      backend: "opfs",
      kind: "hdd",
      format: "raw",
      fileName: "disk1.img",
      sizeBytes: 1024,
      createdAtMs: 0,
    };

    try {
      w.emit({ type: "response", requestId, ok: true, result: { started: true, meta } });
      const handle = await exportPromise;

      let doneSettled = false;
      handle.done.then(
        () => {
          doneSettled = true;
        },
        () => {
          doneSettled = true;
        },
      );

      Object.defineProperty(Object.prototype, "type", { value: "done", configurable: true });

      // Missing `type` must not be satisfied by prototype pollution.
      port.postMessage({ checksumCrc32: "abc" });
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(doneSettled).toBe(false);

      // A valid done message should still resolve.
      port.postMessage({ type: "done", checksumCrc32: "abc" });
      await expect(handle.done).resolves.toEqual({ checksumCrc32: "abc" });
    } finally {
      // Prevent MessagePorts from keeping the event loop alive.
      try {
        port.close();
      } catch {
        // ignore
      }
      if (existing) Object.defineProperty(Object.prototype, "type", existing);
      else delete (Object.prototype as any).type;
      manager.close();
    }
  });
});

