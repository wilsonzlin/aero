import { describe, expect, it } from "vitest";

import { AeroMicCaptureProcessor } from "./mic-worklet-processor.js";
import { createMicRingBuffer, WRITE_POS_INDEX } from "./mic_ring.js";

describe("mic-worklet-processor", () => {
  it("does not throw on malformed SharedArrayBuffer inputs", () => {
    const sab = new SharedArrayBuffer(1);
    const proc = new AeroMicCaptureProcessor({ processorOptions: { ringBuffer: sab } });

    const inputs: Float32Array[][] = [[Float32Array.from([0.25])]];
    const outputs: Float32Array[][] = [[new Float32Array(1)]];
    proc.process(inputs, outputs);

    expect(outputs[0][0]).toEqual(new Float32Array(1));
  });

  it("writes mono input samples into the shared mic ring buffer", () => {
    const rb = createMicRingBuffer(8);
    const proc = new AeroMicCaptureProcessor({ processorOptions: { ringBuffer: rb.sab } });

    const input = Float32Array.from([0.1, 0.2, 0.3]);
    const inputs: Float32Array[][] = [[input]];
    const outputs: Float32Array[][] = [[new Float32Array(input.length)]];
    proc.process(inputs, outputs);

    expect(Atomics.load(rb.header, WRITE_POS_INDEX) >>> 0).toBe(3);
    expect(rb.data.subarray(0, 3)).toEqual(Float32Array.from([0.1, 0.2, 0.3]));
    // The worklet must never leak mic audio to speakers.
    expect(outputs[0][0]).toEqual(new Float32Array(3));
  });
});

