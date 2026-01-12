// Layout-only helpers/constants for the AudioWorklet playback SharedArrayBuffer ring.
//
// IMPORTANT: This file must remain usable from the AudioWorklet global scope.
// Keep it free of `AudioContext` / DOM dependencies.
//
// Vite currently treats modules loaded via `audioWorklet.addModule(new URL(...))` as
// static assets and does not chase their ESM imports, so both:
// - `web/vite.config.ts` (legacy web build)
// - `vite.harness.config.ts` (repo-root harness build)
// manually emit a copy of this file into `dist/assets/` for production builds.
//
// The canonical layout/semantics are also mirrored in Rust:
// `crates/platform/src/audio/worklet_bridge.rs`.

export const READ_FRAME_INDEX = 0;
export const WRITE_FRAME_INDEX = 1;
export const UNDERRUN_COUNT_INDEX = 2;
export const OVERRUN_COUNT_INDEX = 3;

export const HEADER_U32_LEN = 4;
export const HEADER_BYTES = HEADER_U32_LEN * Uint32Array.BYTES_PER_ELEMENT;

export function framesAvailable(readFrameIndex, writeFrameIndex) {
  return (writeFrameIndex - readFrameIndex) >>> 0;
}

export function framesAvailableClamped(readFrameIndex, writeFrameIndex, capacityFrames) {
  return Math.min(framesAvailable(readFrameIndex, writeFrameIndex), capacityFrames);
}

export function framesFree(readFrameIndex, writeFrameIndex, capacityFrames) {
  return capacityFrames - framesAvailableClamped(readFrameIndex, writeFrameIndex, capacityFrames);
}

export function getRingBufferLevelFrames(header, capacityFrames) {
  const read = Atomics.load(header, READ_FRAME_INDEX) >>> 0;
  const write = Atomics.load(header, WRITE_FRAME_INDEX) >>> 0;
  return framesAvailableClamped(read, write, capacityFrames);
}
