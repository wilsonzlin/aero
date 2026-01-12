// Shared scanout descriptor layout.
//
// Keep this file in sync with:
//   crates/aero-shared/src/scanout_state.rs
//
// The state is stored in an `Int32Array` backed by a `SharedArrayBuffer` so it can be accessed
// from JS using `Atomics.*` operations. All values are logically `u32` but must be stored/read
// through `Int32Array` for `Atomics.wait` compatibility; therefore always reinterpret via `>>> 0`.

export const SCANOUT_SOURCE_LEGACY_TEXT = 0 as const;
export const SCANOUT_SOURCE_LEGACY_VBE_LFB = 1 as const;
export const SCANOUT_SOURCE_WDDM = 2 as const;

export const SCANOUT_FORMAT_B8G8R8X8 = 0 as const;

export const SCANOUT_STATE_U32_LEN = 8 as const;
export const SCANOUT_STATE_BYTE_LEN = SCANOUT_STATE_U32_LEN * 4;

export const SCANOUT_STATE_GENERATION_BUSY_BIT = 0x8000_0000 as const;

export const ScanoutStateIndex = {
  GENERATION: 0,
  SOURCE: 1,
  BASE_PADDR_LO: 2,
  BASE_PADDR_HI: 3,
  WIDTH: 4,
  HEIGHT: 5,
  PITCH_BYTES: 6,
  FORMAT: 7,
} as const;

export type ScanoutStateIndex = (typeof ScanoutStateIndex)[keyof typeof ScanoutStateIndex];

export interface ScanoutStateUpdate {
  source: number;
  basePaddrLo: number;
  basePaddrHi: number;
  width: number;
  height: number;
  pitchBytes: number;
  format: number;
}

export interface ScanoutStateSnapshot extends ScanoutStateUpdate {
  generation: number;
}

export function wrapScanoutState(sab: SharedArrayBuffer, byteOffset = 0): Int32Array {
  if (!(sab instanceof SharedArrayBuffer)) {
    throw new TypeError("wrapScanoutState requires a SharedArrayBuffer");
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
  const requiredBytes = off + SCANOUT_STATE_BYTE_LEN;
  if (requiredBytes > sab.byteLength) {
    throw new RangeError(
      `ScanoutState view would be out of bounds: need ${requiredBytes} bytes (offset=${off}, len=${SCANOUT_STATE_BYTE_LEN}), sab.byteLength=${sab.byteLength}`,
    );
  }
  return new Int32Array(sab, off, SCANOUT_STATE_U32_LEN);
}

export function scanoutBasePaddr(snapshot: ScanoutStateSnapshot): bigint {
  return (BigInt(snapshot.basePaddrHi >>> 0) << 32n) | BigInt(snapshot.basePaddrLo >>> 0);
}

export function snapshotScanoutState(words: Int32Array): ScanoutStateSnapshot {
  if (words.length < SCANOUT_STATE_U32_LEN) {
    throw new RangeError(`ScanoutState Int32Array too small: len=${words.length}, need >=${SCANOUT_STATE_U32_LEN}`);
  }

  // Seqlock-style snapshot using a busy bit.
  while (true) {
    const gen0 = Atomics.load(words, ScanoutStateIndex.GENERATION) >>> 0;
    if ((gen0 & SCANOUT_STATE_GENERATION_BUSY_BIT) !== 0) {
      continue;
    }

    const source = Atomics.load(words, ScanoutStateIndex.SOURCE) >>> 0;
    const basePaddrLo = Atomics.load(words, ScanoutStateIndex.BASE_PADDR_LO) >>> 0;
    const basePaddrHi = Atomics.load(words, ScanoutStateIndex.BASE_PADDR_HI) >>> 0;
    const width = Atomics.load(words, ScanoutStateIndex.WIDTH) >>> 0;
    const height = Atomics.load(words, ScanoutStateIndex.HEIGHT) >>> 0;
    const pitchBytes = Atomics.load(words, ScanoutStateIndex.PITCH_BYTES) >>> 0;
    const format = Atomics.load(words, ScanoutStateIndex.FORMAT) >>> 0;

    const gen1 = Atomics.load(words, ScanoutStateIndex.GENERATION) >>> 0;
    if (gen0 !== gen1) {
      continue;
    }

    return {
      generation: gen0,
      source,
      basePaddrLo,
      basePaddrHi,
      width,
      height,
      pitchBytes,
      format,
    };
  }
}

export function publishScanoutState(words: Int32Array, update: ScanoutStateUpdate): number {
  if (words.length < SCANOUT_STATE_U32_LEN) {
    throw new RangeError(`ScanoutState Int32Array too small: len=${words.length}, need >=${SCANOUT_STATE_U32_LEN}`);
  }

  // Acquire the writer lock by setting the busy bit.
  let start = Atomics.load(words, ScanoutStateIndex.GENERATION) >>> 0;
  while (true) {
    if ((start & SCANOUT_STATE_GENERATION_BUSY_BIT) !== 0) {
      start = Atomics.load(words, ScanoutStateIndex.GENERATION) >>> 0;
      continue;
    }
    const desired = (start | SCANOUT_STATE_GENERATION_BUSY_BIT) >>> 0;
    const prev = Atomics.compareExchange(words, ScanoutStateIndex.GENERATION, start | 0, desired | 0) >>> 0;
    if (prev === start) break;
    start = prev;
  }

  // Store the payload fields.
  Atomics.store(words, ScanoutStateIndex.SOURCE, update.source | 0);
  Atomics.store(words, ScanoutStateIndex.BASE_PADDR_LO, update.basePaddrLo | 0);
  Atomics.store(words, ScanoutStateIndex.BASE_PADDR_HI, update.basePaddrHi | 0);
  Atomics.store(words, ScanoutStateIndex.WIDTH, update.width | 0);
  Atomics.store(words, ScanoutStateIndex.HEIGHT, update.height | 0);
  Atomics.store(words, ScanoutStateIndex.PITCH_BYTES, update.pitchBytes | 0);
  Atomics.store(words, ScanoutStateIndex.FORMAT, update.format | 0);

  // Final publish step: increment generation and clear the busy bit.
  const newGeneration = (((start + 1) >>> 0) & (~SCANOUT_STATE_GENERATION_BUSY_BIT >>> 0)) >>> 0;
  Atomics.store(words, ScanoutStateIndex.GENERATION, newGeneration | 0);
  return newGeneration;
}
