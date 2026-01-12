import test from "node:test";
import assert from "node:assert/strict";

import {
  HEADER_U32_LEN,
  OVERRUN_COUNT_INDEX,
  READ_FRAME_INDEX,
  UNDERRUN_COUNT_INDEX,
  WRITE_FRAME_INDEX,
  requiredBytes,
  wrapRingBuffer,
} from "../src/audio/audio_worklet_ring";
import { createAudioOutput, startAudioPerfSampling, writeRingBufferInterleaved } from "../src/platform/audio.ts";

test("AudioOutput exposes getOverrunCount() reading ring buffer header[3]", async () => {
  const originalAudioContext = (globalThis as typeof globalThis & { AudioContext?: unknown }).AudioContext;
  const originalAudioWorkletNode = (globalThis as typeof globalThis & { AudioWorkletNode?: unknown }).AudioWorkletNode;

  class FakeAudioWorklet {
    async addModule(): Promise<void> {}
  }

  class FakeAudioContext {
    readonly sampleRate: number;
    state: AudioContextState = "suspended";
    readonly audioWorklet = new FakeAudioWorklet();
    readonly destination = {};

    constructor(options?: { sampleRate?: number }) {
      this.sampleRate = options?.sampleRate ?? 48_000;
    }

    async resume(): Promise<void> {
      this.state = "running";
    }

    async close(): Promise<void> {
      this.state = "closed";
    }
  }

  class FakeAudioWorkletNode {
    constructor() {}
    connect(): void {}
    disconnect(): void {}
  }

  try {
    (globalThis as typeof globalThis & { AudioContext?: unknown }).AudioContext = FakeAudioContext;
    (globalThis as typeof globalThis & { AudioWorkletNode?: unknown }).AudioWorkletNode = FakeAudioWorkletNode;

    const output = await createAudioOutput({ sampleRate: 48_000 });
    assert.equal(output.enabled, true);
    if (!output.enabled) return;

    Atomics.store(output.ringBuffer.header, OVERRUN_COUNT_INDEX, 123);
    assert.equal(output.getOverrunCount(), 123);
    assert.equal(output.getMetrics().overrunCount, 123);
  } finally {
    if (originalAudioContext === undefined) {
      delete (globalThis as typeof globalThis & { AudioContext?: unknown }).AudioContext;
    } else {
      (globalThis as typeof globalThis & { AudioContext?: unknown }).AudioContext = originalAudioContext;
    }

    if (originalAudioWorkletNode === undefined) {
      delete (globalThis as typeof globalThis & { AudioWorkletNode?: unknown }).AudioWorkletNode;
    } else {
      (globalThis as typeof globalThis & { AudioWorkletNode?: unknown }).AudioWorkletNode = originalAudioWorkletNode;
    }
  }
});

test("createAudioOutput() does not emit unhandledRejection when AudioContext.resume rejects", async () => {
  const originalAudioContext = (globalThis as typeof globalThis & { AudioContext?: unknown }).AudioContext;
  const originalAudioWorkletNode = (globalThis as typeof globalThis & { AudioWorkletNode?: unknown }).AudioWorkletNode;

  class FakeAudioContext {
    readonly sampleRate: number;
    state: AudioContextState = "suspended";
    readonly destination = {};

    constructor(options?: { sampleRate?: number }) {
      this.sampleRate = options?.sampleRate ?? 48_000;
    }

    resume(): Promise<void> {
      return Promise.reject(new Error("resume blocked"));
    }

    async close(): Promise<void> {
      this.state = "closed";
    }
  }

  const unhandled: unknown[] = [];
  const onUnhandled = (reason: unknown) => {
    unhandled.push(reason);
  };

  try {
    (globalThis as typeof globalThis & { AudioContext?: unknown }).AudioContext = FakeAudioContext;

    process.on("unhandledRejection", onUnhandled);

    const output = await createAudioOutput({ sampleRate: 48_000 });
    assert.equal(output.enabled, false);

    // Let promise rejection bookkeeping settle (if any).
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(unhandled.length, 0);
  } finally {
    process.off("unhandledRejection", onUnhandled);

    if (originalAudioContext === undefined) {
      delete (globalThis as typeof globalThis & { AudioContext?: unknown }).AudioContext;
    } else {
      (globalThis as typeof globalThis & { AudioContext?: unknown }).AudioContext = originalAudioContext;
    }

    if (originalAudioWorkletNode === undefined) {
      delete (globalThis as typeof globalThis & { AudioWorkletNode?: unknown }).AudioWorkletNode;
    } else {
      (globalThis as typeof globalThis & { AudioWorkletNode?: unknown }).AudioWorkletNode = originalAudioWorkletNode;
    }
  }
});

test("writeRingBufferInterleaved() increments overrunCount when frames are dropped", () => {
  const capacityFrames = 4;
  const channelCount = 1;

  const buffer = new SharedArrayBuffer(requiredBytes(capacityFrames, channelCount));
  const views = wrapRingBuffer(buffer, capacityFrames, channelCount);

  for (let i = 0; i < HEADER_U32_LEN; i++) Atomics.store(views.header, i, 0);
  Atomics.store(views.header, READ_FRAME_INDEX, 0);
  Atomics.store(views.header, WRITE_FRAME_INDEX, capacityFrames); // Ring buffer is full.
  Atomics.store(views.header, UNDERRUN_COUNT_INDEX, 0);
  Atomics.store(views.header, OVERRUN_COUNT_INDEX, 0);

  const ringBuffer = {
    buffer,
    ...views,
    channelCount,
    capacityFrames,
  };

  const written = writeRingBufferInterleaved(ringBuffer, new Float32Array(2), 48_000, 48_000);
  assert.equal(written, 0);
  assert.equal(Atomics.load(views.header, OVERRUN_COUNT_INDEX) >>> 0, 2);
});

test("startAudioPerfSampling() emits audio.* counters and prefers worklet underrun frames", async () => {
  class FakePort {
    private readonly listeners = new Set<(event: { data: unknown }) => void>();
    addEventListener(type: string, listener: (event: { data: unknown }) => void): void {
      if (type !== "message") return;
      this.listeners.add(listener);
    }
    removeEventListener(type: string, listener: (event: { data: unknown }) => void): void {
      if (type !== "message") return;
      this.listeners.delete(listener);
    }
    start(): void {}
    dispatchMessage(data: unknown): void {
      for (const listener of this.listeners) listener({ data });
    }
  }

  const port = new FakePort();
  const metrics = {
    bufferLevelFrames: 10,
    capacityFrames: 100,
    underrunCount: 1,
    overrunCount: 2,
    sampleRate: 48_000,
    state: "running" as const,
  };

  const output = {
    enabled: true,
    context: { sampleRate: metrics.sampleRate, state: metrics.state },
    node: { port },
    ringBuffer: { capacityFrames: metrics.capacityFrames },
    resume: async () => {},
    close: async () => {},
    writeInterleaved: () => 0,
    getBufferLevelFrames: () => metrics.bufferLevelFrames,
    getUnderrunCount: () => metrics.underrunCount,
    getOverrunCount: () => metrics.overrunCount,
    getMetrics: () => metrics,
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
  } as any;

  const calls: Array<{ name: string; value: number }> = [];
  const perf = {
    counter: (name: string, value: number) => {
      calls.push({ name, value });
    },
  };

  const stop = startAudioPerfSampling(output, perf, 20);
  try {
    assert.deepEqual(calls.slice(0, 4), [
      { name: "audio.bufferLevelFrames", value: 10 },
      { name: "audio.underrunFrames", value: 1 },
      { name: "audio.overrunFrames", value: 2 },
      { name: "audio.sampleRate", value: 48_000 },
    ]);

    port.dispatchMessage({ type: "underrun", underrunFramesTotal: 123 });
    await new Promise((resolve) => setTimeout(resolve, 30));

    const underrunValues = calls.filter((c) => c.name === "audio.underrunFrames").map((c) => c.value);
    assert.equal(underrunValues[0], 1);
    assert.ok(underrunValues.includes(123));

    // Counter is a wrapping u32; ensure perf sampling doesn't clamp via Math.max and
    // can observe a wrap back to a small value.
    port.dispatchMessage({ type: "underrun", underrunFramesTotal: 0xffff_fffe });
    await new Promise((resolve) => setTimeout(resolve, 30));
    port.dispatchMessage({ type: "underrun", underrunFramesTotal: 2 });
    await new Promise((resolve) => setTimeout(resolve, 30));

    const underrunValuesAfterWrap = calls.filter((c) => c.name === "audio.underrunFrames").map((c) => c.value);
    assert.ok(underrunValuesAfterWrap.includes(0xffff_fffe));
    assert.ok(underrunValuesAfterWrap.includes(2));
  } finally {
    stop();
  }
});
