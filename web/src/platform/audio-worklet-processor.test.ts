import { describe, expect, it } from "vitest";

import { AeroAudioProcessor, addUnderrunFrames } from "./audio-worklet-processor.js";
import { requiredBytes, wrapRingBuffer } from "../audio/audio_worklet_ring";
import type { AudioWorkletRingBufferViews } from "../audio/audio_worklet_ring";

function makeRingBuffer(capacityFrames: number, channelCount: number): {
  sab: SharedArrayBuffer;
  views: AudioWorkletRingBufferViews;
} {
  const sab = new SharedArrayBuffer(requiredBytes(capacityFrames, channelCount));
  const views = wrapRingBuffer(sab, capacityFrames, channelCount);
  return {
    sab,
    views,
  };
}

describe("audio-worklet-processor underrun counter", () => {
  it("matches the shared ring-buffer header layout constants", () => {
    // This test intentionally doesn't reach into non-exported constants in
    // `audio-worklet-processor.js`. Instead it asserts the worklet reads/writes
    // the *same* header indices and sample offsets as `audio_worklet_ring.ts`
    // by validating behavior via a minimal render pass.
    const capacityFrames = 1;
    const channelCount = 1;
    const { sab, views } = makeRingBuffer(capacityFrames, channelCount);

    // Seed exactly 1 frame in the buffer and ensure the processor consumes it.
    Atomics.store(views.readIndex, 0, 0);
    Atomics.store(views.writeIndex, 0, 1);
    Atomics.store(views.underrunCount, 0, 0);
    views.samples[0] = 0.5;

    const proc = new AeroAudioProcessor({
      processorOptions: { ringBuffer: sab, channelCount, capacityFrames },
    });

    const outputs: Float32Array[][] = [[new Float32Array(1)]];
    proc.process([], outputs);

    expect(outputs[0][0]).toEqual(Float32Array.from([0.5]));
    expect(Atomics.load(views.readIndex, 0) >>> 0).toBe(1);
    expect(Atomics.load(views.underrunCount, 0) >>> 0).toBe(0);
  });

  it("counts missing frames (not underrun events)", () => {
    const capacityFrames = 4;
    const channelCount = 2;
    const { sab, views } = makeRingBuffer(capacityFrames, channelCount);

    // Two frames available.
    Atomics.store(views.readIndex, 0, 0); // readFrameIndex
    Atomics.store(views.writeIndex, 0, 2); // writeFrameIndex
    Atomics.store(views.underrunCount, 0, 0); // underrunCount

    // Interleaved samples: [L0, R0, L1, R1, ...]
    views.samples.set([0.1, 0.2, 1.1, 1.2]);

    const proc = new AeroAudioProcessor({
      processorOptions: { ringBuffer: sab, channelCount, capacityFrames },
    });

    let lastMessage: unknown = null;
    proc.port.postMessage = (msg: unknown) => {
      lastMessage = msg;
    };

    const framesNeeded = 4;
    const outputs: Float32Array[][] = [[new Float32Array(framesNeeded), new Float32Array(framesNeeded)]];
    proc.process([], outputs);

    expect(outputs[0][0]).toEqual(Float32Array.from([0.1, 1.1, 0, 0]));
    expect(outputs[0][1]).toEqual(Float32Array.from([0.2, 1.2, 0, 0]));

    expect(Atomics.load(views.readIndex, 0) >>> 0).toBe(2);
    expect(Atomics.load(views.underrunCount, 0) >>> 0).toBe(2);
    expect(lastMessage).toEqual({
      type: "underrun",
      underrunFramesAdded: 2,
      underrunFramesTotal: 2,
      underrunCount: 2,
    });

    // Next render quantum: no frames available (fully underrun). The counter should add *frames*.
    lastMessage = null;
    const outputs2: Float32Array[][] = [[new Float32Array(framesNeeded), new Float32Array(framesNeeded)]];
    proc.process([], outputs2);

    expect(outputs2[0][0]).toEqual(new Float32Array(framesNeeded));
    expect(outputs2[0][1]).toEqual(new Float32Array(framesNeeded));

    // Missing 4 more frames -> total 6.
    expect(Atomics.load(views.underrunCount, 0) >>> 0).toBe(6);
    expect(lastMessage).toEqual({
      type: "underrun",
      underrunFramesAdded: 4,
      underrunFramesTotal: 6,
      underrunCount: 6,
    });
  });

  it("wraps the underrun counter as u32", () => {
    const { views } = makeRingBuffer(1, 1);
    Atomics.store(views.underrunCount, 0, 0xffff_fffe);

    const total = addUnderrunFrames(views.header, 4);
    expect(total).toBe(2);
    expect(Atomics.load(views.underrunCount, 0) >>> 0).toBe(2);
  });

  it("does not throw if the provided SharedArrayBuffer is too small/misaligned", () => {
    // Corrupted snapshots or misbehaving hosts should not crash the AudioWorklet. The processor
    // should treat invalid buffers as "no ring attached" and output silence.
    const sab = new SharedArrayBuffer(1);
    const proc = new AeroAudioProcessor({
      processorOptions: { ringBuffer: sab, channelCount: 2, capacityFrames: 4 },
    });

    const outputs: Float32Array[][] = [[new Float32Array(4), new Float32Array(4)]];
    proc.process([], outputs);

    expect(outputs[0][0]).toEqual(new Float32Array(4));
    expect(outputs[0][1]).toEqual(new Float32Array(4));
  });

  it("clamps capacityFrames to the SharedArrayBuffer's actual sample storage", () => {
    const { sab, views } = makeRingBuffer(4, 1);
    Atomics.store(views.readIndex, 0, 0);
    Atomics.store(views.writeIndex, 0, 1);
    views.samples[0] = 0.5;

    // Lie about the capacity via processorOptions; the worklet must not index past the buffer.
    const proc = new AeroAudioProcessor({
      processorOptions: { ringBuffer: sab, channelCount: 1, capacityFrames: 1_000_000 },
    });

    const outputs: Float32Array[][] = [[new Float32Array(1)]];
    proc.process([], outputs);

    expect(outputs[0][0]).toEqual(Float32Array.from([0.5]));
  });
});
