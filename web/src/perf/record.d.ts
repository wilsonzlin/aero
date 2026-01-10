export const PERF_RECORD_SIZE_BYTES: number;

export const PerfRecordType: Readonly<{
  FrameSample: 1;
  GraphicsSample: 2;
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

export type DecodedFrameSampleRecord = {
  type: number;
  workerKind: number;
  frameId: number;
  tUs: number;
  frameUs: number;
  cpuUs: number;
  gpuUs: number;
  ioUs: number;
  jitUs: number;
  instructions: bigint;
  memoryBytes: bigint;
  drawCalls: number;
  ioReadBytes: number;
  ioWriteBytes: number;
};

export type EncodedFrameSampleRecord = {
  workerKind: number;
  frameId: number;
  tUs: number;
  frameUs: number;
  cpuUs: number;
  gpuUs: number;
  ioUs: number;
  jitUs: number;
  instructionsHi: number;
  instructionsLo: number;
  memoryHi: number;
  memoryLo: number;
  drawCalls: number;
  ioReadBytes: number;
  ioWriteBytes: number;
};

export function decodeFrameSampleRecord(view: DataView, byteOffset: number): DecodedFrameSampleRecord;
export function encodeFrameSampleRecord(view: DataView, byteOffset: number, record: EncodedFrameSampleRecord): void;

export function makeEncodedFrameSample(args: {
  workerKind: number;
  frameId: number;
  tUs: number;
  frameMs?: number;
  cpuMs?: number;
  gpuMs?: number;
  ioMs?: number;
  jitMs?: number;
  instructions?: bigint | number;
  memoryBytes?: bigint | number;
  drawCalls?: number;
  ioReadBytes?: number;
  ioWriteBytes?: number;
}): EncodedFrameSampleRecord;

export type DecodedGraphicsSampleRecord = {
  type: number;
  workerKind: number;
  frameId: number;
  tUs: number;
  renderPasses: number;
  pipelineSwitches: number;
  bindGroupChanges: number;
  cpuTranslateUs: number;
  cpuEncodeUs: number;
  uploadBytes: bigint;
  gpuTimeUs: number;
  gpuTimeValid: number;
  gpuTimingSupported: number;
  gpuTimingEnabled: number;
};

export type EncodedGraphicsSampleRecord = {
  workerKind: number;
  frameId: number;
  tUs: number;
  renderPasses: number;
  pipelineSwitches: number;
  bindGroupChanges: number;
  cpuTranslateUs: number;
  cpuEncodeUs: number;
  uploadBytesHi: number;
  uploadBytesLo: number;
  gpuTimeUs: number;
  gpuTimeValid: number;
  gpuTimingSupported: number;
  gpuTimingEnabled: number;
};

export function decodeGraphicsSampleRecord(view: DataView, byteOffset: number): DecodedGraphicsSampleRecord;
export function encodeGraphicsSampleRecord(view: DataView, byteOffset: number, record: EncodedGraphicsSampleRecord): void;

export function makeEncodedGraphicsSample(args: {
  workerKind: number;
  frameId: number;
  tUs: number;
  renderPasses?: number;
  pipelineSwitches?: number;
  bindGroupChanges?: number;
  cpuTranslateMs?: number;
  cpuEncodeMs?: number;
  uploadBytes?: bigint | number;
  gpuTimeMs?: number | null;
  gpuTimingSupported?: boolean;
  gpuTimingEnabled?: boolean;
}): EncodedGraphicsSampleRecord;
