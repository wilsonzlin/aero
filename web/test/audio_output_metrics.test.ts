import test from "node:test";
import assert from "node:assert/strict";

import { createAudioOutput, writeRingBufferInterleaved } from "../src/platform/audio.ts";

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

    Atomics.store(output.ringBuffer.header, 3, 123);
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

test("writeRingBufferInterleaved() increments overrunCount when frames are dropped", () => {
  const capacityFrames = 4;
  const channelCount = 1;

  const headerU32Len = 4;
  const headerBytes = headerU32Len * Uint32Array.BYTES_PER_ELEMENT;
  const sampleCapacity = capacityFrames * channelCount;

  const buffer = new SharedArrayBuffer(headerBytes + sampleCapacity * Float32Array.BYTES_PER_ELEMENT);
  const header = new Uint32Array(buffer, 0, headerU32Len);
  const samples = new Float32Array(buffer, headerBytes, sampleCapacity);

  Atomics.store(header, 0, 0);
  Atomics.store(header, 1, capacityFrames); // Ring buffer is full.
  Atomics.store(header, 2, 0);
  Atomics.store(header, 3, 0);

  const ringBuffer = {
    buffer,
    header,
    readIndex: header.subarray(0, 1),
    writeIndex: header.subarray(1, 2),
    underrunCount: header.subarray(2, 3),
    overrunCount: header.subarray(3, 4),
    samples,
    channelCount,
    capacityFrames,
  };

  const written = writeRingBufferInterleaved(ringBuffer, new Float32Array(2), 48_000, 48_000);
  assert.equal(written, 0);
  assert.equal(Atomics.load(header, 3) >>> 0, 2);
});

