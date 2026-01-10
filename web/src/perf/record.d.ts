export const PERF_RECORD_SIZE_BYTES: number;

export const PerfRecordType: Readonly<{
  FrameSample: 1;
}>;

export const WorkerKind: Readonly<{
  Main: 0;
  CPU: 1;
  GPU: 2;
  IO: 3;
  JIT: 4;
}>;

export function workerKindToString(kind: number): string;

export function u64FromHiLo(hi: number, lo: number): bigint;
export function u64ToHiLo(value: bigint | number): { hi: number; lo: number };

export function msToUsU32(ms: number): number;

export function decodePerfRecord(view: DataView, byteOffset: number): unknown;

