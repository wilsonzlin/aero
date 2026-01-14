import { describe, expect, it } from "vitest";

import { DiskManager } from "./disk_manager";

class MockWorker {
  lastMessage: any;
  lastTransfer: readonly any[] | undefined;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  onmessage: ((event: any) => void) | null = null;

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  postMessage(msg: any, transfer?: any[]): void {
    this.lastMessage = msg;
    this.lastTransfer = transfer;
  }

  terminate(): void {
    // no-op
  }

  emit(data: unknown): void {
    this.onmessage?.({ data });
  }
}

describe("DiskManager.listRemoteCaches", () => {
  it("sends list_remote_caches requests and resolves with the response result", async () => {
    const w = new MockWorker();
    const manager = new DiskManager({ backend: "opfs", worker: w as unknown as Worker });

    const p = manager.listRemoteCaches();

    expect(w.lastMessage).toMatchObject({
      type: "request",
      requestId: 1,
      backend: "opfs",
      op: "list_remote_caches",
      payload: {},
    });

    const result = { ok: true, caches: [], corruptKeys: [] };
    w.emit({ type: "response", requestId: 1, ok: true, result });

    await expect(p).resolves.toEqual(result);
    manager.close();
  });
});

