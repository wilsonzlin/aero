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

    constructor(public readonly specifier: unknown, public readonly opts: unknown) {}

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
          // Only complete after a tick to model vsync-paced submissions.
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
});
