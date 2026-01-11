export const WRITE_POS_INDEX: number;
export const READ_POS_INDEX: number;
export const DROPPED_SAMPLES_INDEX: number;
export const CAPACITY_SAMPLES_INDEX: number;

export const HEADER_U32_LEN: number;
export const HEADER_BYTES: number;

export type MicRingBuffer = {
  sab: SharedArrayBuffer;
  header: Uint32Array;
  data: Float32Array;
  capacity: number;
};

export function samplesAvailable(readPos: number, writePos: number): number;
export function samplesAvailableClamped(readPos: number, writePos: number, capacity: number): number;
export function samplesFree(readPos: number, writePos: number, capacity: number): number;

export function createMicRingBuffer(capacitySamples: number): MicRingBuffer;

export function micRingBufferReadInto(
  rb: Pick<MicRingBuffer, "header" | "data" | "capacity">,
  out: Float32Array,
): number;

export function micRingBufferWrite(
  rb: Pick<MicRingBuffer, "header" | "data" | "capacity">,
  samples: Float32Array,
): number;

