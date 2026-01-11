import { describe, expect, it, vi } from "vitest";

import { JitWorkerClient } from "./jit_worker_client";

type MessageListener = (event: { data: unknown }) => void;
type GenericListener = (event: any) => void;

class FakeWorker {
  readonly messageListeners = new Set<MessageListener>();
  readonly errorListeners = new Set<GenericListener>();
  readonly messageErrorListeners = new Set<GenericListener>();
  readonly postMessageCalls: Array<{ msg: unknown; transfer: Transferable[] }> = [];

  addEventListener(type: string, listener: GenericListener): void {
    if (type === "message") {
      this.messageListeners.add(listener as MessageListener);
    } else if (type === "error") {
      this.errorListeners.add(listener);
    } else if (type === "messageerror") {
      this.messageErrorListeners.add(listener);
    }
  }

  removeEventListener(type: string, listener: GenericListener): void {
    if (type === "message") {
      this.messageListeners.delete(listener as MessageListener);
    } else if (type === "error") {
      this.errorListeners.delete(listener);
    } else if (type === "messageerror") {
      this.messageErrorListeners.delete(listener);
    }
  }

  postMessage(msg: unknown, transfer: Transferable[] = []): void {
    this.postMessageCalls.push({ msg, transfer });
  }

  dispatchMessage(data: unknown): void {
    for (const listener of this.messageListeners) {
      listener({ data });
    }
  }

  dispatchError(message: string): void {
    for (const listener of this.errorListeners) {
      listener({ message });
    }
  }

  dispatchMessageError(): void {
    for (const listener of this.messageErrorListeners) {
      listener({});
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
      // Attach the rejection handler *before* advancing timers so Node doesn't
      // treat the timeout rejection as an unhandled promise rejection.
      const assertion = expect(promise).rejects.toThrow(/Timed out/i);
      await vi.advanceTimersByTimeAsync(50);
      await assertion;
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

  it("rejects pending requests when the worker errors", async () => {
    const worker = new FakeWorker();
    const client = new JitWorkerClient(worker as unknown as Worker);

    const promise = client.compile(new ArrayBuffer(8), { timeoutMs: 1000 });
    worker.dispatchError("worker died");
    await expect(promise).rejects.toThrow(/worker died/i);
  });

  it("rejects pending requests on messageerror", async () => {
    const worker = new FakeWorker();
    const client = new JitWorkerClient(worker as unknown as Worker);

    const promise = client.compile(new ArrayBuffer(8), { timeoutMs: 1000 });
    worker.dispatchMessageError();
    await expect(promise).rejects.toThrow(/deserialization/i);
  });

  it("destroy() rejects pending requests and prevents new compiles", async () => {
    const worker = new FakeWorker();
    const client = new JitWorkerClient(worker as unknown as Worker);

    const pending = client.compile(new ArrayBuffer(8), { timeoutMs: 1000 });
    client.destroy(new Error("gone"));
    await expect(pending).rejects.toThrow("gone");

    await expect(client.compile(new ArrayBuffer(8), { timeoutMs: 1000 })).rejects.toThrow(/destroyed/i);
  });
});
