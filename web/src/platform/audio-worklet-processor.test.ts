import { describe, expect, it } from "vitest";

import { AeroAudioProcessor, addUnderrunFrames } from "./audio-worklet-processor.js";
import { READ_FRAME_INDEX, UNDERRUN_COUNT_INDEX, WRITE_FRAME_INDEX, requiredBytes, wrapRingBuffer } from "../audio/audio_worklet_ring";

function makeRingBuffer(capacityFrames: number, channelCount: number): {
  sab: SharedArrayBuffer;
  header: Uint32Array;
  samples: Float32Array;
} {
  const sab = new SharedArrayBuffer(requiredBytes(capacityFrames, channelCount));
  const views = wrapRingBuffer(sab, capacityFrames, channelCount);
  return {
    sab,
    header: views.header,
    samples: views.samples,
  };
}

describe("audio-worklet-processor underrun counter", () => {
  it("counts missing frames (not underrun events)", () => {
    const capacityFrames = 4;
    const channelCount = 2;
    const { sab, header, samples } = makeRingBuffer(capacityFrames, channelCount);

    // Two frames available.
    Atomics.store(header, READ_FRAME_INDEX, 0); // readFrameIndex
    Atomics.store(header, WRITE_FRAME_INDEX, 2); // writeFrameIndex
    Atomics.store(header, UNDERRUN_COUNT_INDEX, 0); // underrunCount

    // Interleaved samples: [L0, R0, L1, R1, ...]
    samples.set([0.1, 0.2, 1.1, 1.2]);

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

    expect(Atomics.load(header, READ_FRAME_INDEX) >>> 0).toBe(2);
    expect(Atomics.load(header, UNDERRUN_COUNT_INDEX) >>> 0).toBe(2);
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
    expect(Atomics.load(header, UNDERRUN_COUNT_INDEX) >>> 0).toBe(6);
    expect(lastMessage).toEqual({
      type: "underrun",
      underrunFramesAdded: 4,
      underrunFramesTotal: 6,
      underrunCount: 6,
    });
  });

  it("wraps the underrun counter as u32", () => {
    const { header } = makeRingBuffer(1, 1);
    Atomics.store(header, UNDERRUN_COUNT_INDEX, 0xffff_fffe);

    const total = addUnderrunFrames(header, 4);
    expect(total).toBe(2);
    expect(Atomics.load(header, UNDERRUN_COUNT_INDEX) >>> 0).toBe(2);
  });
});
