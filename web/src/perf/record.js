export const PERF_RECORD_SIZE_BYTES = 64;

export const PerfRecordType = Object.freeze({
  FrameSample: 1,
});

export const WorkerKind = Object.freeze({
  Main: 0,
  CPU: 1,
  GPU: 2,
  IO: 3,
  JIT: 4,
});

export function workerKindToString(kind) {
  switch (kind) {
    case WorkerKind.Main:
      return "main";
    case WorkerKind.CPU:
      return "cpu";
    case WorkerKind.GPU:
      return "gpu";
    case WorkerKind.IO:
      return "io";
    case WorkerKind.JIT:
      return "jit";
    default:
      return `worker(${kind})`;
  }
}

export function u64FromHiLo(hi, lo) {
  return (BigInt(hi >>> 0) << 32n) | BigInt(lo >>> 0);
}

export function u64ToHiLo(value) {
  if (typeof value === "number") {
    if (!Number.isFinite(value) || value < 0) {
      throw new Error(`u64 number must be finite and non-negative (got ${value})`);
    }
    if (!Number.isSafeInteger(value)) {
      throw new Error(`u64 number must be a safe integer; pass bigint instead (got ${value})`);
    }
    value = BigInt(value);
  }
  if (typeof value !== "bigint") {
    throw new Error(`u64 must be a number or bigint (got ${typeof value})`);
  }
  if (value < 0n || value > 0xffff_ffff_ffff_ffffn) {
    throw new Error(`u64 bigint out of range (got ${value})`);
  }
  const lo = Number(value & 0xffff_ffffn) >>> 0;
  const hi = Number((value >> 32n) & 0xffff_ffffn) >>> 0;
  return { hi, lo };
}

function clampU32(value, name) {
  if (!Number.isFinite(value) || value < 0) {
    return 0;
  }
  const rounded = Math.round(value);
  if (rounded > 0xffff_ffff) {
    // Saturate rather than wrap: consumers treat this as a timestamp/duration.
    return 0xffff_ffff;
  }
  return rounded >>> 0;
}

export function msToUsU32(ms) {
  return clampU32(ms * 1000, "ms");
}

export function decodePerfRecord(view, byteOffset) {
  const type = view.getUint32(byteOffset + 0, true);
  if (type === PerfRecordType.FrameSample) {
    return decodeFrameSampleRecord(view, byteOffset);
  }
  return { type };
}

export function decodeFrameSampleRecord(view, byteOffset) {
  const type = view.getUint32(byteOffset + 0, true);
  const workerKind = view.getUint32(byteOffset + 4, true);
  const frameId = view.getUint32(byteOffset + 8, true);
  const tUs = view.getUint32(byteOffset + 12, true);

  const frameUs = view.getUint32(byteOffset + 16, true);
  const cpuUs = view.getUint32(byteOffset + 20, true);
  const gpuUs = view.getUint32(byteOffset + 24, true);
  const ioUs = view.getUint32(byteOffset + 28, true);
  const jitUs = view.getUint32(byteOffset + 32, true);

  const instructionsLo = view.getUint32(byteOffset + 36, true);
  const instructionsHi = view.getUint32(byteOffset + 40, true);

  const memoryLo = view.getUint32(byteOffset + 44, true);
  const memoryHi = view.getUint32(byteOffset + 48, true);

  const drawCalls = view.getUint32(byteOffset + 52, true);
  const ioReadBytes = view.getUint32(byteOffset + 56, true);
  const ioWriteBytes = view.getUint32(byteOffset + 60, true);

  return {
    type,
    workerKind,
    frameId,
    tUs,
    frameUs,
    cpuUs,
    gpuUs,
    ioUs,
    jitUs,
    instructions: u64FromHiLo(instructionsHi, instructionsLo),
    memoryBytes: u64FromHiLo(memoryHi, memoryLo),
    drawCalls,
    ioReadBytes,
    ioWriteBytes,
  };
}

export function encodeFrameSampleRecord(view, byteOffset, record) {
  // Header
  view.setUint32(byteOffset + 0, PerfRecordType.FrameSample, true);
  view.setUint32(byteOffset + 4, record.workerKind >>> 0, true);
  view.setUint32(byteOffset + 8, record.frameId >>> 0, true);
  view.setUint32(byteOffset + 12, record.tUs >>> 0, true);

  // Durations (microseconds)
  view.setUint32(byteOffset + 16, record.frameUs >>> 0, true);
  view.setUint32(byteOffset + 20, record.cpuUs >>> 0, true);
  view.setUint32(byteOffset + 24, record.gpuUs >>> 0, true);
  view.setUint32(byteOffset + 28, record.ioUs >>> 0, true);
  view.setUint32(byteOffset + 32, record.jitUs >>> 0, true);

  // Counters
  view.setUint32(byteOffset + 36, record.instructionsLo >>> 0, true);
  view.setUint32(byteOffset + 40, record.instructionsHi >>> 0, true);
  view.setUint32(byteOffset + 44, record.memoryLo >>> 0, true);
  view.setUint32(byteOffset + 48, record.memoryHi >>> 0, true);
  view.setUint32(byteOffset + 52, record.drawCalls >>> 0, true);
  view.setUint32(byteOffset + 56, record.ioReadBytes >>> 0, true);
  view.setUint32(byteOffset + 60, record.ioWriteBytes >>> 0, true);
}

export function makeEncodedFrameSample({
  workerKind,
  frameId,
  tUs,
  frameMs = 0,
  cpuMs = 0,
  gpuMs = 0,
  ioMs = 0,
  jitMs = 0,
  instructions = 0n,
  memoryBytes = 0n,
  drawCalls = 0,
  ioReadBytes = 0,
  ioWriteBytes = 0,
}) {
  const { hi: instructionsHi, lo: instructionsLo } = u64ToHiLo(instructions);
  const { hi: memoryHi, lo: memoryLo } = u64ToHiLo(memoryBytes);

  return {
    workerKind: workerKind >>> 0,
    frameId: frameId >>> 0,
    tUs: tUs >>> 0,
    frameUs: msToUsU32(frameMs),
    cpuUs: msToUsU32(cpuMs),
    gpuUs: msToUsU32(gpuMs),
    ioUs: msToUsU32(ioMs),
    jitUs: msToUsU32(jitMs),
    instructionsHi,
    instructionsLo,
    memoryHi,
    memoryLo,
    drawCalls: clampU32(drawCalls, "drawCalls"),
    ioReadBytes: clampU32(ioReadBytes, "ioReadBytes"),
    ioWriteBytes: clampU32(ioWriteBytes, "ioWriteBytes"),
  };
}

