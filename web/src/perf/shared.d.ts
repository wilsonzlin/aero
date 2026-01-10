export type PerfChannel = {
  schemaVersion: number;
  runStartEpochMs: number;
  capacity: number;
  recordSize: number;
  buffers: Record<number, SharedArrayBuffer>;
};

export function nowEpochMs(): number;

export function createPerfChannel(options?: {
  capacity?: number;
  workerKinds?: number[];
}): PerfChannel;

