import type { AudioRingBufferLayout } from "./audio";
import { clampReadFrameIndexToCapacity } from "../audio/audio_worklet_ring";

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
 *   layout, we proceed without throwing. For safety (and to match the Rust
 *   restore implementation), indices are clamped against the smaller of the
 *   snapshot's capacity and the actual ring capacity.
 */
export function restoreAudioWorkletRing(ring: AudioRingBufferLayout, state: AudioWorkletRingStateLike): void {
  // Clear sample contents first, so any subsequently-consumed frames are silent.
  ring.samples.fill(0);

  const ringCapacityFrames = ring.capacityFrames >>> 0;

  // Treat all snapshot fields as wrapping u32 values.
  const snapshotCapacityFrames = state.capacityFrames >>> 0;
  const effectiveCapacityFrames =
    snapshotCapacityFrames !== 0 ? Math.min(snapshotCapacityFrames, ringCapacityFrames) : ringCapacityFrames;

  const writePos = state.writePos >>> 0;
  const readPos = clampReadFrameIndexToCapacity(state.readPos, writePos, effectiveCapacityFrames);

  Atomics.store(ring.readIndex, 0, readPos);
  Atomics.store(ring.writeIndex, 0, writePos);
}
