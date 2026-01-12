import {
  HEADER_BYTES,
  HEADER_U32_LEN,
  OVERRUN_COUNT_INDEX,
  READ_FRAME_INDEX,
  UNDERRUN_COUNT_INDEX,
  WRITE_FRAME_INDEX,
  framesAvailable,
  framesAvailableClamped,
  framesFree,
  getRingBufferLevelFrames,
} from "../platform/audio_worklet_ring_layout.js";

export {
  HEADER_BYTES,
  HEADER_U32_LEN,
  OVERRUN_COUNT_INDEX,
  READ_FRAME_INDEX,
  UNDERRUN_COUNT_INDEX,
  WRITE_FRAME_INDEX,
  framesAvailable,
  framesAvailableClamped,
  framesFree,
  getRingBufferLevelFrames,
};

export type AudioWorkletRingBufferViews = {
  header: Uint32Array;
  readIndex: Uint32Array;
  writeIndex: Uint32Array;
  underrunCount: Uint32Array;
  overrunCount: Uint32Array;
  samples: Float32Array;
};

/**
 * Clamp a read frame counter to a consistent state when the producer has advanced
 * by more than the ring can hold.
 *
 * This mirrors the logic in the Rust snapshot restore path
 * (`crates/platform/src/audio/worklet_bridge.rs`).
 */
export function clampReadFrameIndexToCapacity(
  readFrameIndex: number,
  writeFrameIndex: number,
  capacityFrames: number,
): number {
  const cap = capacityFrames >>> 0;
  const read = readFrameIndex >>> 0;
  const write = writeFrameIndex >>> 0;
  if (cap === 0) return read;
  const available = framesAvailable(read, write);
  if (available > cap) return (write - cap) >>> 0;
  return read;
}

export function requiredBytes(capacityFrames: number, channelCount: number): number {
  const sampleCapacity = capacityFrames * channelCount;
  return HEADER_BYTES + sampleCapacity * Float32Array.BYTES_PER_ELEMENT;
}

export function wrapRingBuffer(
  sab: SharedArrayBuffer,
  capacityFrames: number,
  channelCount: number,
): AudioWorkletRingBufferViews {
  const sampleCapacity = capacityFrames * channelCount;
  const bytes = requiredBytes(capacityFrames, channelCount);
  if (sab.byteLength < bytes) {
    throw new Error(`Provided ring buffer is too small: need ${bytes} bytes, got ${sab.byteLength} bytes`);
  }

  const header = new Uint32Array(sab, 0, HEADER_U32_LEN);
  const samples = new Float32Array(sab, HEADER_BYTES, sampleCapacity);

  return {
    header,
    readIndex: header.subarray(READ_FRAME_INDEX, READ_FRAME_INDEX + 1),
    writeIndex: header.subarray(WRITE_FRAME_INDEX, WRITE_FRAME_INDEX + 1),
    underrunCount: header.subarray(UNDERRUN_COUNT_INDEX, UNDERRUN_COUNT_INDEX + 1),
    overrunCount: header.subarray(OVERRUN_COUNT_INDEX, OVERRUN_COUNT_INDEX + 1),
    samples,
  };
}
