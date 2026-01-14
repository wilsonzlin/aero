import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { perf } from "../perf/perf";
import { GPU_PROTOCOL_NAME, GPU_PROTOCOL_VERSION } from "../ipc/gpu-protocol";

const overlay = vi.hoisted(() => ({
  getSnapshot: null as null | (() => unknown),
}));

vi.mock("../../ui/debug_overlay.ts", () => {
  return {
    DebugOverlay: class DebugOverlayMock {
      constructor(getSnapshot: () => unknown) {
        overlay.getSnapshot = getSnapshot;
      }
      show() {}
      detach() {}
    },
  };
});

describe("main/frameScheduler (telemetry)", () => {
  type Posted = { message: unknown; transfer?: unknown[] };

  let posted: Posted[] = [];
  let rafCallback: ((time: number) => void) | null = null;

  const originalRaf = globalThis.requestAnimationFrame;
  const originalCancel = globalThis.cancelAnimationFrame;

  beforeEach(() => {
    posted = [];
    rafCallback = null;
    overlay.getSnapshot = null;

    vi.spyOn(perf, "registerWorker").mockImplementation(() => 0);
    vi.spyOn(console, "error").mockImplementation(() => {});
    vi.spyOn(console, "warn").mockImplementation(() => {});
    vi.spyOn(console, "info").mockImplementation(() => {});

    globalThis.requestAnimationFrame = ((cb: (time: number) => void) => {
      rafCallback = cb;
      return 1;
    }) as unknown as typeof globalThis.requestAnimationFrame;
    globalThis.cancelAnimationFrame = (() => {}) as unknown as typeof globalThis.cancelAnimationFrame;
  });

  afterEach(() => {
    globalThis.requestAnimationFrame = originalRaf;
    globalThis.cancelAnimationFrame = originalCancel;
    vi.restoreAllMocks();
  });

  function makeMockWorker(): Worker & { dispatch: (data: unknown) => void } {
    let onMessage: ((event: MessageEvent<unknown>) => void) | null = null;

    return {
      postMessage(message: unknown, transfer?: unknown[]) {
        posted.push({ message, transfer });
      },
      addEventListener(type: string, cb: EventListenerOrEventListenerObject) {
        if (type === "message") onMessage = cb as unknown as (event: MessageEvent<unknown>) => void;
      },
      removeEventListener(type: string, cb: EventListenerOrEventListenerObject) {
        if (type === "message" && onMessage === (cb as unknown as (event: MessageEvent<unknown>) => void)) {
          onMessage = null;
        }
      },
      dispatch(data: unknown) {
        onMessage?.({ data } as MessageEvent<unknown>);
      },
    } as unknown as Worker & { dispatch: (data: unknown) => void };
  }

  it("preserves gpuEvents across metrics updates", async () => {
    const { startFrameScheduler } = await import("./frameScheduler");

    const gpuWorker = makeMockWorker();
    const sharedFrameState = new SharedArrayBuffer(8 * Int32Array.BYTES_PER_ELEMENT);
    const sharedFramebuffer = new SharedArrayBuffer(64);

    const handle = startFrameScheduler({
      gpuWorker,
      sharedFrameState,
      sharedFramebuffer,
      showDebugOverlay: true,
    });

    expect(typeof rafCallback).toBe("function");
    expect(typeof overlay.getSnapshot).toBe("function");

    gpuWorker.dispatch({
      protocol: GPU_PROTOCOL_NAME,
      protocolVersion: GPU_PROTOCOL_VERSION,
      type: "metrics",
      framesReceived: 1,
      framesPresented: 1,
      framesDropped: 0,
      telemetry: { hello: "world" },
    });

    const snapAfterFirstMetrics = overlay.getSnapshot?.() as any;
    expect(snapAfterFirstMetrics.hello).toBe("world");
    expect(snapAfterFirstMetrics.framesReceived).toBe(1);
    expect(snapAfterFirstMetrics.gpuEvents).toBeUndefined();

    gpuWorker.dispatch({
      protocol: GPU_PROTOCOL_NAME,
      protocolVersion: GPU_PROTOCOL_VERSION,
      type: "events",
      version: 1,
      events: [
        {
          time_ms: 0,
          backend_kind: "mock",
          severity: "error",
          category: "Test",
          message: "boom",
        },
      ],
    });

    const snapAfterEvents = overlay.getSnapshot?.() as any;
    expect(Array.isArray(snapAfterEvents?.gpuEvents)).toBe(true);
    expect(snapAfterEvents.gpuEvents).toHaveLength(1);
    expect(snapAfterEvents.hello).toBe("world");
    expect(console.error).toHaveBeenCalled();

    gpuWorker.dispatch({
      protocol: GPU_PROTOCOL_NAME,
      protocolVersion: GPU_PROTOCOL_VERSION,
      type: "metrics",
      framesReceived: 1,
      framesPresented: 1,
      framesDropped: 0,
      telemetry: { hello: "world2" },
    });

    const snapAfterMetrics = overlay.getSnapshot?.() as any;
    expect(snapAfterMetrics.hello).toBe("world2");
    expect(snapAfterMetrics.framesReceived).toBe(1);
    expect(Array.isArray(snapAfterMetrics?.gpuEvents)).toBe(true);
    expect(snapAfterMetrics.gpuEvents).toHaveLength(1);

    handle.stop();
  });

  it("preserves gpuStats across metrics updates", async () => {
    const { startFrameScheduler } = await import("./frameScheduler");

    const gpuWorker = makeMockWorker();
    const sharedFrameState = new SharedArrayBuffer(8 * Int32Array.BYTES_PER_ELEMENT);
    const sharedFramebuffer = new SharedArrayBuffer(64);

    const handle = startFrameScheduler({
      gpuWorker,
      sharedFrameState,
      sharedFramebuffer,
      showDebugOverlay: true,
    });

    expect(typeof overlay.getSnapshot).toBe("function");

    gpuWorker.dispatch({
      protocol: GPU_PROTOCOL_NAME,
      protocolVersion: GPU_PROTOCOL_VERSION,
      type: "stats",
      version: 1,
      timeMs: 0,
      backendKind: "webgpu",
      counters: {
        presents_attempted: 2,
        presents_succeeded: 1,
        recoveries_attempted: 3,
        recoveries_succeeded: 1,
        surface_reconfigures: 4,
      },
    });

    const snapAfterStats = overlay.getSnapshot?.() as any;
    expect(snapAfterStats.gpuStats?.type).toBe("stats");
    expect(snapAfterStats.gpuStats?.backendKind).toBe("webgpu");
    expect(snapAfterStats.gpuStats?.counters?.recoveries_attempted).toBe(3);

    gpuWorker.dispatch({
      protocol: GPU_PROTOCOL_NAME,
      protocolVersion: GPU_PROTOCOL_VERSION,
      type: "metrics",
      framesReceived: 1,
      framesPresented: 1,
      framesDropped: 0,
      telemetry: { hello: "world" },
    });

    const snapAfterMetrics = overlay.getSnapshot?.() as any;
    expect(snapAfterMetrics.hello).toBe("world");
    expect(snapAfterMetrics.gpuStats?.backendKind).toBe("webgpu");

    handle.stop();
  });
});
