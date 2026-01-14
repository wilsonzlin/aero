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

describe("DiskManager.pruneRemoteCaches", () => {
  it("sends prune_remote_caches requests and resolves with the response result", async () => {
    const w = new MockWorker();
    const manager = new DiskManager({ backend: "opfs", worker: w as unknown as Worker });

    const p = manager.pruneRemoteCaches({ olderThanMs: 4000, maxCaches: 3 });

    expect(w.lastMessage).toMatchObject({
      type: "request",
      requestId: 1,
      backend: "opfs",
      op: "prune_remote_caches",
      payload: { olderThanMs: 4000, maxCaches: 3 },
    });

    const result = { ok: true, pruned: 2, examined: 5 };
    w.emit({ type: "response", requestId: 1, ok: true, result });

    await expect(p).resolves.toEqual(result);
    manager.close();
  });

  it("returns prunedKeys when dryRun is enabled", async () => {
    const w = new MockWorker();
    const manager = new DiskManager({ backend: "opfs", worker: w as unknown as Worker });

    const p = manager.pruneRemoteCaches({ olderThanMs: 4000, dryRun: true });

    expect(w.lastMessage).toMatchObject({
      type: "request",
      requestId: 1,
      backend: "opfs",
      op: "prune_remote_caches",
      payload: { olderThanMs: 4000, dryRun: true },
    });

    const result = { ok: true, pruned: 1, examined: 2, prunedKeys: ["cache1"] };
    w.emit({ type: "response", requestId: 1, ok: true, result });

    const resp = await p;
    expect(resp.prunedKeys).toEqual(["cache1"]);
    expect(resp).toEqual(result);
    manager.close();
  });
});

