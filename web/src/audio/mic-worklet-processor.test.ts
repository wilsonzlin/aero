import { describe, expect, it } from "vitest";

import { AeroMicCaptureProcessor } from "./mic-worklet-processor.js";
import {
  CAPACITY_SAMPLES_INDEX,
  DROPPED_SAMPLES_INDEX,
  HEADER_BYTES,
  HEADER_U32_LEN,
  READ_POS_INDEX,
  createMicRingBuffer,
  WRITE_POS_INDEX,
} from "./mic_ring.js";

describe("mic-worklet-processor", () => {
  it("does not throw on malformed SharedArrayBuffer inputs", () => {
    const sab = new SharedArrayBuffer(1);
    const proc = new AeroMicCaptureProcessor({ processorOptions: { ringBuffer: sab } });

    const inputs: Float32Array[][] = [[Float32Array.from([0.25])]];
    const outputs: Float32Array[][] = [[new Float32Array(1)]];
    proc.process(inputs, outputs);

    expect(outputs[0][0]).toEqual(new Float32Array(1));
  });

  it("treats over-large ring buffers as invalid (caps to avoid pathological memory usage)", () => {
    // Allocate a mic ring buffer that's just over the supported max capacity. This should not crash
    // the worklet, but it should refuse to treat it as a valid sink.
    const maxPlusOne = 1_048_576 + 1;
    const sab = new SharedArrayBuffer(HEADER_BYTES + maxPlusOne * 4);
    const header = new Uint32Array(sab, 0, HEADER_U32_LEN);
    // Old/legacy rings may not populate CAPACITY_SAMPLES_INDEX; leave it 0 so the processor derives
    // capacity from SAB size.
    Atomics.store(header, WRITE_POS_INDEX, 0);
    Atomics.store(header, READ_POS_INDEX, 0);
    Atomics.store(header, DROPPED_SAMPLES_INDEX, 0);
    Atomics.store(header, CAPACITY_SAMPLES_INDEX, 0);

    const proc = new AeroMicCaptureProcessor({ processorOptions: { ringBuffer: sab } });
    const inputs: Float32Array[][] = [[Float32Array.from([0.25])]];
    const outputs: Float32Array[][] = [[new Float32Array(1)]];
    proc.process(inputs, outputs);

    expect(Atomics.load(header, WRITE_POS_INDEX) >>> 0).toBe(0);
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
