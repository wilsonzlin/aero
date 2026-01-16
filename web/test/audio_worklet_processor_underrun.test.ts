import test from "node:test";
import assert from "node:assert/strict";

import {
  READ_FRAME_INDEX,
  UNDERRUN_COUNT_INDEX,
  WRITE_FRAME_INDEX,
  requiredBytes,
  wrapRingBuffer,
} from "../src/audio/audio_worklet_ring";
import { AeroAudioProcessor, addUnderrunFrames } from "../src/platform/audio-worklet-processor.js";

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

test("AudioWorklet processor underrun counter increments by missing frames", async () => {
  const capacityFrames = 4;
  const channelCount = 2;
  const { sab, header, samples } = makeRingBuffer(capacityFrames, channelCount);

  // Two frames available.
  Atomics.store(header, READ_FRAME_INDEX, 0); // readFrameIndex
  Atomics.store(header, WRITE_FRAME_INDEX, 2); // writeFrameIndex
  Atomics.store(header, UNDERRUN_COUNT_INDEX, 0); // underrunCount (missing frames)

  // Interleaved samples: [L0, R0, L1, R1, ...]
  samples.set([0.1, 0.2, 1.1, 1.2]);

  const proc = new AeroAudioProcessor({
    processorOptions: {
      ringBuffer: sab,
      channelCount,
      capacityFrames,
      sendUnderrunMessages: true,
      underrunMessageIntervalMs: 1,
    },
  });

  let lastMessage: unknown = null;
  proc.port.postMessage = (msg: unknown) => {
    lastMessage = msg;
  };

  const framesNeeded = 4;
  const outputs: Float32Array[][] = [[new Float32Array(framesNeeded), new Float32Array(framesNeeded)]];
  proc.process([], outputs);

  // Compare as Float32Arrays so the expected values are rounded the same way.
  assert.deepEqual(outputs[0][0], Float32Array.from([0.1, 1.1, 0, 0]));
  assert.deepEqual(outputs[0][1], Float32Array.from([0.2, 1.2, 0, 0]));
  assert.equal(Atomics.load(header, READ_FRAME_INDEX) >>> 0, 2);
  assert.equal(Atomics.load(header, UNDERRUN_COUNT_INDEX) >>> 0, 2);
  assert.deepEqual(lastMessage, {
    type: "underrun",
    underrunFramesAdded: 2,
    underrunFramesTotal: 2,
    underrunCount: 2,
  });

  // Next quantum: buffer empty, so we should add 4 more missing frames.
  lastMessage = null;
  // Underrun messages are rate-limited; ensure wall time advances beyond the interval.
  await new Promise((r) => setTimeout(r, 2));
  const outputs2: Float32Array[][] = [[new Float32Array(framesNeeded), new Float32Array(framesNeeded)]];
  proc.process([], outputs2);

  assert.deepEqual(Array.from(outputs2[0][0]), [0, 0, 0, 0]);
  assert.deepEqual(Array.from(outputs2[0][1]), [0, 0, 0, 0]);
  assert.equal(Atomics.load(header, UNDERRUN_COUNT_INDEX) >>> 0, 6);
  assert.deepEqual(lastMessage, {
    type: "underrun",
    underrunFramesAdded: 4,
    underrunFramesTotal: 6,
    underrunCount: 6,
  });
});

test("addUnderrunFrames wraps as u32", () => {
  const { header } = makeRingBuffer(1, 1);
  Atomics.store(header, UNDERRUN_COUNT_INDEX, 0xffff_fffe);
  const total = addUnderrunFrames(header, 4);
  assert.equal(total, 2);
  assert.equal(Atomics.load(header, UNDERRUN_COUNT_INDEX) >>> 0, 2);
});
