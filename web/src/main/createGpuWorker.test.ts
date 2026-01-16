import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { perf } from "../perf/perf";
import { GPU_PROTOCOL_NAME, GPU_PROTOCOL_VERSION } from "../ipc/gpu-protocol";
import { createGpuWorker } from "./createGpuWorker";

describe("main/createGpuWorker", () => {
  type Posted = { message: unknown; transfer?: unknown[] };

  const GPU_MESSAGE_BASE = { protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION } as const;

  let posted: Posted[] = [];
  let originalWorker: unknown;

  class MockWorker {
    private readonly messageListeners: Array<(event: MessageEvent<unknown>) => void> = [];
    private readonly errorListeners: Array<(event: ErrorEvent) => void> = [];

    private pendingSubmit: { requestId: number; fence: bigint } | null = null;

    readonly specifier: unknown;
    readonly opts: unknown;

    constructor(specifier: unknown, opts: unknown) {
      this.specifier = specifier;
      this.opts = opts;
    }

    postMessage(message: unknown, transfer?: unknown[]): void {
      posted.push({ message, transfer });
      const m = message as { type?: unknown };
      switch (m.type) {
        case "init": {
          // Synchronously ACK init for deterministic tests.
          this.dispatchMessage({ ...GPU_MESSAGE_BASE, type: "ready", backendKind: "headless" });
          break;
        }
        case "submit_aerogpu": {
          const req = message as { requestId: number; signalFence: bigint };
          this.pendingSubmit = { requestId: req.requestId, fence: req.signalFence };
          break;
        }
        case "tick": {
          if (!this.pendingSubmit) break;
          const { requestId, fence } = this.pendingSubmit;
          this.pendingSubmit = null;
          // Only complete after a tick to model a worker that requires periodic ticks for progress.
          this.dispatchMessage({ ...GPU_MESSAGE_BASE, type: "submit_complete", requestId, completedFence: fence });
          break;
        }
        default:
          break;
      }
    }

    addEventListener(type: string, listener: unknown): void {
      const cb = listener as (event: unknown) => void;
      if (type === "message") this.messageListeners.push(cb as (event: MessageEvent<unknown>) => void);
      if (type === "error") this.errorListeners.push(cb as (event: ErrorEvent) => void);
    }

    removeEventListener(type: string, listener: unknown): void {
      const cb = listener as (event: unknown) => void;
      if (type === "message") {
        const idx = this.messageListeners.indexOf(cb as (event: MessageEvent<unknown>) => void);
        if (idx >= 0) this.messageListeners.splice(idx, 1);
      }
      if (type === "error") {
        const idx = this.errorListeners.indexOf(cb as (event: ErrorEvent) => void);
        if (idx >= 0) this.errorListeners.splice(idx, 1);
      }
    }

    terminate(): void {}

    private dispatchMessage(data: unknown): void {
      const event = { data } as MessageEvent<unknown>;
      for (const cb of this.messageListeners) cb(event);
    }
  }

  beforeEach(() => {
    posted = [];
    originalWorker = globalThis.Worker;
    // `createGpuWorker()` is a main-thread helper; in unit tests we stub out both the worker and
    // perf plumbing so the code remains deterministic in the Node test environment.
    globalThis.Worker = MockWorker as unknown as typeof Worker;
    vi.spyOn(perf, "registerWorker").mockImplementation(() => 0);
  });

  afterEach(() => {
    globalThis.Worker = originalWorker as typeof Worker;
    vi.restoreAllMocks();
  });

  it("pumps ticks while waiting for submit_complete", async () => {
    vi.useFakeTimers();

    const canvas = {
      transferControlToOffscreen: () => ({}) as unknown as OffscreenCanvas,
    } as unknown as HTMLCanvasElement;

    const handle = createGpuWorker({ canvas, width: 2, height: 2, devicePixelRatio: 1 });
    await handle.ready;

    const submitPromise = handle.submitAerogpu(new ArrayBuffer(4), 1n);

    // No tick should be posted until the pump fires.
    expect(posted.some((p) => (p.message as { type?: unknown }).type === "tick")).toBe(false);

    await vi.advanceTimersByTimeAsync(20);
    const completed = await submitPromise;
    expect(completed.completedFence).toBe(1n);

    const tickCount = posted.filter((p) => (p.message as { type?: unknown }).type === "tick").length;
    expect(tickCount).toBeGreaterThan(0);

    // The pump should stop once there are no in-flight submits.
    await vi.advanceTimersByTimeAsync(100);
    const tickCountAfter = posted.filter((p) => (p.message as { type?: unknown }).type === "tick").length;
    expect(tickCountAfter).toBe(tickCount);

    handle.shutdown();
    vi.useRealTimers();
  });

  it("forwards submitAerogpu opts (flags/engineId)", async () => {
    vi.useFakeTimers();

    const canvas = {
      transferControlToOffscreen: () => ({}) as unknown as OffscreenCanvas,
    } as unknown as HTMLCanvasElement;

    const handle = createGpuWorker({ canvas, width: 2, height: 2, devicePixelRatio: 1 });
    await handle.ready;

    const submitPromise = handle.submitAerogpu(new ArrayBuffer(4), 1n, undefined, 0, { flags: 0x12, engineId: 3 });
    // The submit message is posted in a `ready.then(...)` microtask; yield once so it runs.
    await Promise.resolve();

    let submitMsg: { flags?: unknown; engineId?: unknown } | undefined = undefined;
    for (let i = posted.length - 1; i >= 0; i -= 1) {
      const msg = posted[i]?.message as { type?: unknown; flags?: unknown; engineId?: unknown } | undefined;
      if (msg?.type === "submit_aerogpu") {
        submitMsg = msg;
        break;
      }
    }
    expect(submitMsg?.flags).toBe(0x12);
    expect(submitMsg?.engineId).toBe(3);

    await vi.advanceTimersByTimeAsync(20);
    await expect(submitPromise).resolves.toMatchObject({ completedFence: 1n });

    handle.shutdown();
    vi.useRealTimers();
  });

  it("falls back to posting submit_aerogpu without a transfer list when transfers are rejected", async () => {
    let submitAttempts = 0;
    let sawTransferAttempt = false;
    let sawFallbackAttempt = false;

    class RejectTransferWorker extends MockWorker {
      override postMessage(message: unknown, transfer?: unknown[]): void {
        const m = message as { type?: unknown };
        if (m.type === "submit_aerogpu") {
          submitAttempts += 1;
          const hasTransfer = Array.isArray(transfer) && transfer.length > 0;
          if (hasTransfer) sawTransferAttempt = true;
          else sawFallbackAttempt = true;
          if (hasTransfer) {
            throw new Error("transfer list rejected");
          }
        }
        super.postMessage(message, transfer);
      }
    }

    globalThis.Worker = RejectTransferWorker as unknown as typeof Worker;

    vi.useFakeTimers();

    const canvas = {
      transferControlToOffscreen: () => ({}) as unknown as OffscreenCanvas,
    } as unknown as HTMLCanvasElement;

    const handle = createGpuWorker({ canvas, width: 2, height: 2, devicePixelRatio: 1 });
    await handle.ready;

    const submitPromise = handle.submitAerogpu(new ArrayBuffer(4), 1n);

    // Allow submit to be posted and tick pump to run.
    await vi.advanceTimersByTimeAsync(20);
    await expect(submitPromise).resolves.toMatchObject({ completedFence: 1n });

    expect(submitAttempts).toBe(2);
    expect(sawTransferAttempt).toBe(true);
    expect(sawFallbackAttempt).toBe(true);

    handle.shutdown();
    vi.useRealTimers();
  });

  it("rejects submitAerogpu if shutdown races with submit registration", async () => {
    vi.useFakeTimers();

    const canvas = {
      transferControlToOffscreen: () => ({}) as unknown as OffscreenCanvas,
    } as unknown as HTMLCanvasElement;

    const handle = createGpuWorker({ canvas, width: 2, height: 2, devicePixelRatio: 1 });
    await handle.ready;

    const submitPromise = handle.submitAerogpu(new ArrayBuffer(4), 1n);
    handle.shutdown();

    await expect(submitPromise).rejects.toThrow(/shutdown/i);

    // Ensure the pump timer is cancelled and does not continue posting ticks after shutdown.
    await vi.advanceTimersByTimeAsync(50);
    expect(posted.some((p) => (p.message as { type?: unknown }).type === "tick")).toBe(false);

    vi.useRealTimers();
  });

  it("rejects ready if shutdown occurs before the worker becomes ready", async () => {
    class NoReadyWorker extends MockWorker {
      override postMessage(message: unknown, transfer?: unknown[]): void {
        posted.push({ message, transfer });
        // Intentionally do not emit `type:"ready"` in response to init.
      }
    }

    globalThis.Worker = NoReadyWorker as unknown as typeof Worker;

    const canvas = {
      transferControlToOffscreen: () => ({}) as unknown as OffscreenCanvas,
    } as unknown as HTMLCanvasElement;

    const handle = createGpuWorker({ canvas, width: 2, height: 2, devicePixelRatio: 1 });
    const readyPromise = handle.ready;
    handle.shutdown();

    await expect(readyPromise).rejects.toThrow(/shutdown/i);
  });

  it("does not throw if the worker rejects the shutdown postMessage", async () => {
    class ThrowOnShutdownWorker extends MockWorker {
      override postMessage(message: unknown, transfer?: unknown[]): void {
        const m = message as { type?: unknown };
        if (m.type === "shutdown") {
          throw new Error("shutdown rejected");
        }
        super.postMessage(message, transfer);
      }
    }

    globalThis.Worker = ThrowOnShutdownWorker as unknown as typeof Worker;

    const canvas = {
      transferControlToOffscreen: () => ({}) as unknown as OffscreenCanvas,
    } as unknown as HTMLCanvasElement;

    const handle = createGpuWorker({ canvas, width: 2, height: 2, devicePixelRatio: 1 });
    await handle.ready;

    expect(() => handle.shutdown()).not.toThrow();
  });
});
