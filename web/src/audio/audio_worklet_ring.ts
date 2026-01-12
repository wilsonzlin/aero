export * from "../platform/audio_worklet_ring_layout.js";

import {
  HEADER_BYTES,
  HEADER_U32_LEN,
  OVERRUN_COUNT_INDEX,
  READ_FRAME_INDEX,
  UNDERRUN_COUNT_INDEX,
  WRITE_FRAME_INDEX,
  framesAvailable,
} from "../platform/audio_worklet_ring_layout.js";

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

/**
 * Hard caps for the AudioWorklet playback ring.
 *
 * These mirror the caps enforced by the Rust `WorkletBridge` implementation
 * (`crates/platform/src/audio/worklet_bridge.rs`) so that:
 * - The browser runtime does not allocate multi-gigabyte `SharedArrayBuffer`s.
 * - The JS surface validates inputs similarly to the wasm surface.
 */
export const MAX_AUDIO_WORKLET_RING_CAPACITY_FRAMES = 1_048_576; // 2^20 frames (~21s @ 48kHz)
export const MAX_AUDIO_WORKLET_RING_CHANNEL_COUNT = 2;

function assertValidU32(name: string, value: number): number {
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new Error(`invalid ${name}: ${value}`);
  }
  if (value > 0xffff_ffff) {
    throw new Error(`invalid ${name}: ${value}`);
  }
  return value >>> 0;
}

function assertValidCapacityFrames(capacityFrames: number): number {
  const cap = assertValidU32("capacityFrames", capacityFrames);
  if (cap > MAX_AUDIO_WORKLET_RING_CAPACITY_FRAMES) {
    throw new Error(`capacityFrames must be <= ${MAX_AUDIO_WORKLET_RING_CAPACITY_FRAMES}`);
  }
  return cap;
}

function assertValidChannelCount(channelCount: number): number {
  const cc = assertValidU32("channelCount", channelCount);
  if (cc > MAX_AUDIO_WORKLET_RING_CHANNEL_COUNT) {
    throw new Error(`channelCount must be <= ${MAX_AUDIO_WORKLET_RING_CHANNEL_COUNT}`);
  }
  return cc;
}

export function requiredBytes(capacityFrames: number, channelCount: number): number {
  const cap = assertValidCapacityFrames(capacityFrames);
  const cc = assertValidChannelCount(channelCount);
  const sampleCapacity = cap * cc;
  return HEADER_BYTES + sampleCapacity * Float32Array.BYTES_PER_ELEMENT;
}

export function wrapRingBuffer(
  sab: SharedArrayBuffer,
  capacityFrames: number,
  channelCount: number,
): AudioWorkletRingBufferViews {
  const cap = assertValidCapacityFrames(capacityFrames);
  const cc = assertValidChannelCount(channelCount);
  const sampleCapacity = cap * cc;
  const bytes = requiredBytes(cap, cc);
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
