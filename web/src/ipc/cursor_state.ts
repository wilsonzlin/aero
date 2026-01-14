// Shared hardware cursor descriptor layout.
//
// Keep this file in sync with:
//   crates/aero-shared/src/cursor_state.rs
//
// The state is stored in an `Int32Array` backed by a `SharedArrayBuffer` so it can be accessed
// from JS using `Atomics.*` operations.
//
// All fields are stored as 32-bit words. Most are logically `u32` but must be stored/read through
// `Int32Array` for `Atomics.wait` compatibility; therefore always reinterpret via `>>> 0`.
//
// Cursor position fields (`x`, `y`) are logically signed `i32` (allow negative coordinates) and
// should be read via `| 0`.

import { AerogpuFormat } from "../../../emulator/protocol/aerogpu/aerogpu_pci.ts";

// Cursor format values use the AeroGPU `AerogpuFormat` numeric (`u32`) discriminants.
//
// Semantics (from the AeroGPU protocol):
// - `*X8*` formats (`B8G8R8X8*`, `R8G8B8X8*`) do not carry alpha. When converting to RGBA
//   (e.g. for cursor blending), treat alpha as fully opaque (`0xff`) and ignore the stored
//   `X` byte.
// - `*_SRGB` variants are layout-identical to their UNORM counterparts; only the color space
//   interpretation differs. Presenters must avoid double-applying gamma when handling sRGB
//   cursor formats.
export const CURSOR_FORMAT_B8G8R8A8 = AerogpuFormat.B8G8R8A8Unorm;
export const CURSOR_FORMAT_B8G8R8X8 = AerogpuFormat.B8G8R8X8Unorm;
export const CURSOR_FORMAT_R8G8B8A8 = AerogpuFormat.R8G8B8A8Unorm;
export const CURSOR_FORMAT_R8G8B8X8 = AerogpuFormat.R8G8B8X8Unorm;

export const CURSOR_STATE_U32_LEN = 12 as const;
export const CURSOR_STATE_BYTE_LEN = CURSOR_STATE_U32_LEN * 4;

export const CURSOR_STATE_GENERATION_BUSY_BIT = 0x8000_0000 as const;

export const CursorStateIndex = {
  GENERATION: 0,
  ENABLE: 1,
  X: 2,
  Y: 3,
  HOT_X: 4,
  HOT_Y: 5,
  WIDTH: 6,
  HEIGHT: 7,
  PITCH_BYTES: 8,
  FORMAT: 9,
  BASE_PADDR_LO: 10,
  BASE_PADDR_HI: 11,
} as const;

export type CursorStateIndex = (typeof CursorStateIndex)[keyof typeof CursorStateIndex];

export interface CursorStateUpdate {
  enable: number;
  x: number;
  y: number;
  hotX: number;
  hotY: number;
  width: number;
  height: number;
  pitchBytes: number;
  format: number;
  basePaddrLo: number;
  basePaddrHi: number;
}

export interface CursorStateSnapshot extends CursorStateUpdate {
  generation: number;
}

export function wrapCursorState(sab: SharedArrayBuffer, byteOffset = 0): Int32Array {
  if (!(sab instanceof SharedArrayBuffer)) {
    throw new TypeError("wrapCursorState requires a SharedArrayBuffer");
  }
  if (!Number.isFinite(byteOffset)) {
    throw new RangeError(`byteOffset must be a finite number, got ${String(byteOffset)}`);
  }
  const off = Math.trunc(byteOffset);
  if (off < 0) {
    throw new RangeError(`byteOffset must be >= 0, got ${off}`);
  }
  if (off % 4 !== 0) {
    throw new RangeError(`byteOffset must be 4-byte aligned, got ${off}`);
  }
  const requiredBytes = off + CURSOR_STATE_BYTE_LEN;
  if (requiredBytes > sab.byteLength) {
    throw new RangeError(
      `CursorState view would be out of bounds: need ${requiredBytes} bytes (offset=${off}, len=${CURSOR_STATE_BYTE_LEN}), sab.byteLength=${sab.byteLength}`,
    );
  }
  return new Int32Array(sab, off, CURSOR_STATE_U32_LEN);
}

export function cursorBasePaddr(snapshot: CursorStateSnapshot): bigint {
  return (BigInt(snapshot.basePaddrHi >>> 0) << 32n) | BigInt(snapshot.basePaddrLo >>> 0);
}

export function snapshotCursorState(words: Int32Array): CursorStateSnapshot {
  if (words.length < CURSOR_STATE_U32_LEN) {
    throw new RangeError(`CursorState Int32Array too small: len=${words.length}, need >=${CURSOR_STATE_U32_LEN}`);
  }

  // Seqlock-style snapshot using a busy bit.
  while (true) {
    const gen0 = Atomics.load(words, CursorStateIndex.GENERATION) >>> 0;
    if ((gen0 & CURSOR_STATE_GENERATION_BUSY_BIT) !== 0) {
      continue;
    }

    const enable = Atomics.load(words, CursorStateIndex.ENABLE) >>> 0;
    const x = Atomics.load(words, CursorStateIndex.X) | 0;
    const y = Atomics.load(words, CursorStateIndex.Y) | 0;
    const hotX = Atomics.load(words, CursorStateIndex.HOT_X) >>> 0;
    const hotY = Atomics.load(words, CursorStateIndex.HOT_Y) >>> 0;
    const width = Atomics.load(words, CursorStateIndex.WIDTH) >>> 0;
    const height = Atomics.load(words, CursorStateIndex.HEIGHT) >>> 0;
    const pitchBytes = Atomics.load(words, CursorStateIndex.PITCH_BYTES) >>> 0;
    const format = Atomics.load(words, CursorStateIndex.FORMAT) >>> 0;
    const basePaddrLo = Atomics.load(words, CursorStateIndex.BASE_PADDR_LO) >>> 0;
    const basePaddrHi = Atomics.load(words, CursorStateIndex.BASE_PADDR_HI) >>> 0;

    const gen1 = Atomics.load(words, CursorStateIndex.GENERATION) >>> 0;
    if (gen0 !== gen1) {
      continue;
    }

    return {
      generation: gen0,
      enable,
      x,
      y,
      hotX,
      hotY,
      width,
      height,
      pitchBytes,
      format,
      basePaddrLo,
      basePaddrHi,
    };
  }
}

export function publishCursorState(words: Int32Array, update: CursorStateUpdate): number {
  if (words.length < CURSOR_STATE_U32_LEN) {
    throw new RangeError(`CursorState Int32Array too small: len=${words.length}, need >=${CURSOR_STATE_U32_LEN}`);
  }

  // Acquire the writer lock by setting the busy bit.
  let start = Atomics.load(words, CursorStateIndex.GENERATION) >>> 0;
  while (true) {
    if ((start & CURSOR_STATE_GENERATION_BUSY_BIT) !== 0) {
      start = Atomics.load(words, CursorStateIndex.GENERATION) >>> 0;
      continue;
    }
    const desired = (start | CURSOR_STATE_GENERATION_BUSY_BIT) >>> 0;
    const prev = Atomics.compareExchange(words, CursorStateIndex.GENERATION, start | 0, desired | 0) >>> 0;
    if (prev === start) break;
    start = prev;
  }

  // Store the payload fields.
  Atomics.store(words, CursorStateIndex.ENABLE, update.enable | 0);
  Atomics.store(words, CursorStateIndex.X, update.x | 0);
  Atomics.store(words, CursorStateIndex.Y, update.y | 0);
  Atomics.store(words, CursorStateIndex.HOT_X, update.hotX | 0);
  Atomics.store(words, CursorStateIndex.HOT_Y, update.hotY | 0);
  Atomics.store(words, CursorStateIndex.WIDTH, update.width | 0);
  Atomics.store(words, CursorStateIndex.HEIGHT, update.height | 0);
  Atomics.store(words, CursorStateIndex.PITCH_BYTES, update.pitchBytes | 0);
  Atomics.store(words, CursorStateIndex.FORMAT, update.format | 0);
  Atomics.store(words, CursorStateIndex.BASE_PADDR_LO, update.basePaddrLo | 0);
  Atomics.store(words, CursorStateIndex.BASE_PADDR_HI, update.basePaddrHi | 0);

  // Final publish step: increment generation and clear the busy bit.
  const newGeneration = (((start + 1) >>> 0) & (~CURSOR_STATE_GENERATION_BUSY_BIT >>> 0)) >>> 0;
  Atomics.store(words, CursorStateIndex.GENERATION, newGeneration | 0);
  return newGeneration;
}
