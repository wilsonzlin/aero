export type PerfChannel = {
  schemaVersion: number;
  runStartEpochMs: number;
  capacity: number;
  recordSize: number;
  frameHeader: SharedArrayBuffer;
  buffers: Record<number, SharedArrayBuffer>;
};

export const PERF_FRAME_HEADER_FRAME_ID_INDEX: number;
export const PERF_FRAME_HEADER_T_US_INDEX: number;
export const PERF_FRAME_HEADER_I32_LEN: number;

export function nowEpochMs(): number;

export function createPerfChannel(options?: {
  capacity?: number;
  workerKinds?: number[];
}): PerfChannel;
