import type { AudioRingBufferLayout } from "./audio";
import { READ_FRAME_INDEX, WRITE_FRAME_INDEX, clampReadFrameIndexToCapacity } from "../audio/audio_worklet_ring";

/**
 * Snapshot state for the AudioWorklet playback ring buffer.
 *
 * This is compatible with `aero_io_snapshot::io::audio::state::AudioWorkletRingState`
 * (Rust), but uses JS-friendly field names.
 */
export type AudioWorkletRingStateLike = {
  /**
   * Ring capacity in frames (per channel) at the time the snapshot was taken.
   *
   * If 0, the capacity is unknown and should be ignored.
   */
  capacityFrames: number;
  /**
   * Monotonic read frame counter (wrapping `u32`).
   */
  readPos: number;
  /**
   * Monotonic write frame counter (wrapping `u32`).
   */
  writePos: number;
};

/**
 * Restore playback ring buffer indices from snapshot state.
 *
 * Notes:
 * - The snapshot preserves *indices only*; ring sample contents are cleared to
 *   silence on restore to avoid replaying stale host audio.
 * - If `state.capacityFrames` is non-zero and does not match the provided ring
 *   layout, we ignore it and proceed. JS cannot resize an existing
 *   SharedArrayBuffer, so clamping indices against the actual ring capacity is
 *   the safest behaviour and ensures progress immediately.
 */
export function restoreAudioWorkletRing(ring: AudioRingBufferLayout, state: AudioWorkletRingStateLike): void {
  // Clear sample contents first, so any subsequently-consumed frames are silent.
  ring.samples.fill(0);

  const ringCapacityFrames = ring.capacityFrames >>> 0;

  // Treat all snapshot fields as wrapping u32 values.
  const snapshotCapacityFrames = state.capacityFrames >>> 0;
  if (snapshotCapacityFrames !== 0 && snapshotCapacityFrames !== ringCapacityFrames) {
    // Intentionally ignored; see function doc comment.
  }

  const writePos = state.writePos >>> 0;
  const readPos = clampReadFrameIndexToCapacity(state.readPos, writePos, ringCapacityFrames);

  Atomics.store(ring.header, READ_FRAME_INDEX, readPos);
  Atomics.store(ring.header, WRITE_FRAME_INDEX, writePos);
}
