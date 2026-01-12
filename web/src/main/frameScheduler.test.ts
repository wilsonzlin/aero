import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { perf } from "../perf/perf";
import { FRAME_PRESENTED, FRAME_STATUS_INDEX, GPU_PROTOCOL_NAME, GPU_PROTOCOL_VERSION } from "../ipc/gpu-protocol";
import {
  SCANOUT_SOURCE_LEGACY_TEXT,
  SCANOUT_SOURCE_WDDM,
  SCANOUT_STATE_U32_LEN,
  ScanoutStateIndex,
} from "../ipc/scanout_state";
import { startFrameScheduler } from "./frameScheduler";

describe("main/frameScheduler", () => {
  type Posted = { message: unknown; transfer?: unknown[] };

  let rafCallback: ((time: number) => void) | null = null;
  let posted: Posted[] = [];

  const originalRaf = globalThis.requestAnimationFrame;
  const originalCancel = globalThis.cancelAnimationFrame;

  beforeEach(() => {
    posted = [];
    rafCallback = null;

    vi.spyOn(perf, "registerWorker").mockImplementation(() => 0);

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

  function makeMockWorker(): Worker {
    return {
      postMessage(message: unknown, transfer?: unknown[]) {
        posted.push({ message, transfer });
      },
      addEventListener() {},
      removeEventListener() {},
    } as unknown as Worker;
  }

  it("sends ticks while scanout source is WDDM even when frame state is PRESENTED", () => {
    const gpuWorker = makeMockWorker();

    const sharedFrameState = new SharedArrayBuffer(8 * Int32Array.BYTES_PER_ELEMENT);
    const frameState = new Int32Array(sharedFrameState);
    Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_PRESENTED);

    const sharedFramebuffer = new SharedArrayBuffer(64);

    const scanoutState = new SharedArrayBuffer(SCANOUT_STATE_U32_LEN * 4);
    const scanoutWords = new Int32Array(scanoutState, 0, SCANOUT_STATE_U32_LEN);
    Atomics.store(scanoutWords, ScanoutStateIndex.GENERATION, 0);
    Atomics.store(scanoutWords, ScanoutStateIndex.SOURCE, SCANOUT_SOURCE_WDDM);

    const handle = startFrameScheduler({
      gpuWorker,
      sharedFrameState,
      sharedFramebuffer,
      sharedFramebufferOffsetBytes: 0,
      scanoutState,
      scanoutStateOffsetBytes: 0,
      showDebugOverlay: false,
    });

    expect(rafCallback).not.toBeNull();
    rafCallback?.(0);

    const tickCount = posted.filter((p) => (p.message as { protocol?: unknown; protocolVersion?: unknown; type?: unknown })?.type === "tick")
      .length;
    expect(tickCount).toBe(1);

    // Sanity-check that the scheduler still posts a correctly versioned init message.
    const initMsg = posted.find((p) => (p.message as { type?: unknown }).type === "init")?.message as
      | { protocol?: unknown; protocolVersion?: unknown }
      | undefined;
    expect(initMsg?.protocol).toBe(GPU_PROTOCOL_NAME);
    expect(initMsg?.protocolVersion).toBe(GPU_PROTOCOL_VERSION);

    handle.stop();
  });

  it("wakes the GPU worker on scanout generation changes even when the shared framebuffer is idle", () => {
    const gpuWorker = makeMockWorker();

    const sharedFrameState = new SharedArrayBuffer(8 * Int32Array.BYTES_PER_ELEMENT);
    const frameState = new Int32Array(sharedFrameState);
    Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_PRESENTED);

    const sharedFramebuffer = new SharedArrayBuffer(64);

    const scanoutState = new SharedArrayBuffer(SCANOUT_STATE_U32_LEN * 4);
    const scanoutWords = new Int32Array(scanoutState, 0, SCANOUT_STATE_U32_LEN);
    Atomics.store(scanoutWords, ScanoutStateIndex.GENERATION, 0);
    Atomics.store(scanoutWords, ScanoutStateIndex.SOURCE, SCANOUT_SOURCE_LEGACY_TEXT);

    const handle = startFrameScheduler({
      gpuWorker,
      sharedFrameState,
      sharedFramebuffer,
      sharedFramebufferOffsetBytes: 0,
      scanoutState,
      scanoutStateOffsetBytes: 0,
      showDebugOverlay: false,
    });

    expect(rafCallback).not.toBeNull();
    rafCallback?.(0);
    expect(posted.some((p) => (p.message as { type?: unknown }).type === "tick")).toBe(false);

    // Bump scanout generation; scheduler should post a tick on the next frame even though
    // the shared framebuffer remained PRESENTED.
    Atomics.store(scanoutWords, ScanoutStateIndex.GENERATION, 1);
    rafCallback?.(1);
    expect(posted.filter((p) => (p.message as { type?: unknown }).type === "tick")).toHaveLength(1);

    // No further generation change; should not spam ticks while idle.
    rafCallback?.(2);
    expect(posted.filter((p) => (p.message as { type?: unknown }).type === "tick")).toHaveLength(1);

    handle.stop();
  });
});

