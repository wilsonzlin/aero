import { describe, expect, it, vi } from "vitest";

import { JitWorkerClient } from "../../../src/workers/jit_worker_client";

type MessageListener = (event: { data: unknown }) => void;

class FakeWorker {
  readonly listeners = new Set<MessageListener>();
  readonly postMessageCalls: Array<{ msg: unknown; transfer: Transferable[] }> = [];

  addEventListener(type: string, listener: MessageListener): void {
    if (type !== "message") return;
    this.listeners.add(listener);
  }

  postMessage(msg: unknown, transfer: Transferable[] = []): void {
    this.postMessageCalls.push({ msg, transfer });
  }

  dispatchMessage(data: unknown): void {
    for (const listener of this.listeners) {
      listener({ data });
    }
  }
}

describe("JitWorkerClient", () => {
  it("sends jit:compile and resolves with the matching response", async () => {
    const worker = new FakeWorker();
    const client = new JitWorkerClient(worker as unknown as Worker);

    const wasmBytes = new ArrayBuffer(8);
    const promise = client.compile(wasmBytes, { timeoutMs: 1000 });

    expect(worker.postMessageCalls).toHaveLength(1);
    const { msg } = worker.postMessageCalls[0]!;
    expect(msg).toMatchObject({ type: "jit:compile", wasmBytes });
    const id = (msg as { id: number }).id;

    const response = { type: "jit:compiled", id, module: {}, durationMs: 1.23 };
    worker.dispatchMessage(response);

    await expect(promise).resolves.toEqual(response);
  });

  it("rejects on timeout", async () => {
    vi.useFakeTimers();
    try {
      const worker = new FakeWorker();
      const client = new JitWorkerClient(worker as unknown as Worker);

      const promise = client.compile(new ArrayBuffer(8), { timeoutMs: 10 });
      await vi.advanceTimersByTimeAsync(50);
      await expect(promise).rejects.toThrow(/Timed out/i);
    } finally {
      vi.useRealTimers();
    }
  });

  it("rejects when postMessage throws", async () => {
    class ThrowWorker extends FakeWorker {
      override postMessage(): void {
        throw new Error("boom");
      }
    }

    const worker = new ThrowWorker();
    const client = new JitWorkerClient(worker as unknown as Worker);
    await expect(client.compile(new ArrayBuffer(8), { timeoutMs: 1000 })).rejects.toThrow("boom");
  });
});

