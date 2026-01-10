// Aero IPC shared-memory layout (TypeScript).
//
// This mirrors `crates/aero-ipc/src/layout.rs`.
//
// All integers are little-endian, and all ring-buffer control fields are 32-bit
// words so they can be driven via `Atomics` on an `Int32Array`.

export const IPC_MAGIC = 0x4350_4941; // "AIPC" LE
export const IPC_VERSION = 1;

export const RECORD_ALIGN = 4;
export const WRAP_MARKER = 0xffff_ffff;

export const ringCtrl = {
  HEAD: 0,
  TAIL_RESERVE: 1,
  TAIL_COMMIT: 2,
  CAPACITY: 3,
  WORDS: 4,
  BYTES: 16,
} as const;

export const ipcHeader = {
  WORDS: 4,
  BYTES: 16,
  MAGIC: 0,
  VERSION: 1,
  TOTAL_BYTES: 2,
  QUEUE_COUNT: 3,
} as const;

export const queueDesc = {
  WORDS: 4,
  BYTES: 16,
  KIND: 0,
  OFFSET_BYTES: 1,
  CAPACITY_BYTES: 2,
  RESERVED: 3,
} as const;

export const queueKind = {
  CMD: 0,
  EVT: 1,
} as const;

export function alignUp(value: number, align: number): number {
  if ((align & (align - 1)) !== 0) throw new Error("align must be power of two");
  return (value + (align - 1)) & ~(align - 1);
}

