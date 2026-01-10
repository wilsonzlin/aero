import type {
  MicrobenchCaseName,
  MicrobenchCaseResultV1,
  MicrobenchMode,
  MicrobenchSuiteOptions,
  MicrobenchSuiteResultV1,
} from "./types";
import { clearBenchmarks, setBenchmark } from "./store";

import initMicrobench, {
  bench_branchy,
  bench_hash,
  bench_integer_alu,
  bench_memcpy,
} from "../wasm/aero_microbench/aero_microbench.js";

let wasmInit: Promise<void> | null = null;

async function ensureMicrobenchWasm(): Promise<void> {
  if (!wasmInit) {
    wasmInit = initMicrobench().then(() => undefined);
  }
  await wasmInit;
}

function nowMs(): number {
  return performance.now();
}

function checksumToString(value: unknown): string {
  if (typeof value === "bigint") {
    return value.toString(10);
  }
  if (typeof value === "number") {
    return Math.trunc(value).toString(10);
  }
  return String(value);
}

function clampU32(value: number): number {
  if (!Number.isFinite(value)) {
    return 0xffff_ffff;
  }
  if (value < 0) {
    return 0;
  }
  if (value > 0xffff_ffff) {
    return 0xffff_ffff;
  }
  return Math.floor(value);
}

function defaultOptions(): Required<MicrobenchSuiteOptions> {
  return {
    mode: "timeBudget",
    warmup: true,
    timeBudgetMs: 250,
    integerAluIters: 5_000_000,
    branchyIters: 5_000_000,
    memcpyBytes: 4 * 1024 * 1024,
    memcpyIters: 64,
    hashBytes: 1024 * 1024,
    hashIters: 8,
  };
}

function resolveOptions(
  opts: MicrobenchSuiteOptions | undefined,
): Required<MicrobenchSuiteOptions> {
  return { ...defaultOptions(), ...(opts ?? {}) };
}

function measureUnary(fn: (iters: number) => unknown, iters: number): [number, string] {
  const t0 = nowMs();
  const checksum = fn(iters);
  const t1 = nowMs();
  return [t1 - t0, checksumToString(checksum)];
}

function measureBinary(
  fn: (bytes: number, iters: number) => unknown,
  bytes: number,
  iters: number,
): [number, string] {
  const t0 = nowMs();
  const checksum = fn(bytes, iters);
  const t1 = nowMs();
  return [t1 - t0, checksumToString(checksum)];
}

function chooseItersForTimeBudget(
  measure: (iters: number) => number,
  timeBudgetMs: number,
): number {
  // Scale until the measurement is large enough to be meaningful (avoid 0ms).
  const minProbeMs = Math.min(20, timeBudgetMs / 2);
  let probeIters = 1024;
  let probeMs = 0;
  for (let i = 0; i < 16; i++) {
    probeMs = measure(probeIters);
    if (probeMs >= minProbeMs) {
      break;
    }
    probeIters = clampU32(probeIters * 2);
    if (probeIters === 0xffff_ffff) {
      break;
    }
  }

  if (probeMs <= 0) {
    return probeIters;
  }

  return clampU32((probeIters * timeBudgetMs) / probeMs);
}

function mkCaseResult(
  name: MicrobenchCaseName,
  durationMs: number,
  iters: number,
  checksum: string,
  bytes?: number,
): MicrobenchCaseResultV1 {
  const durationSec = durationMs / 1000;
  const safeDurationSec = durationSec > 0 ? durationSec : 1e-9;
  if (bytes === undefined) {
    return {
      name,
      duration_ms: durationMs,
      params: { iters },
      checksum,
      throughput: {
        unit: "iters_per_sec",
        value: iters / safeDurationSec,
      },
    };
  }

  const totalBytes = bytes * iters;
  return {
    name,
    duration_ms: durationMs,
    params: { iters, bytes },
    checksum,
    throughput: {
      unit: "bytes_per_sec",
      value: totalBytes / safeDurationSec,
    },
  };
}

async function runUnaryCase(
  name: MicrobenchCaseName,
  fn: (iters: number) => unknown,
  mode: MicrobenchMode,
  warmup: boolean,
  timeBudgetMs: number,
  fixedIters: number,
): Promise<MicrobenchCaseResultV1> {
  let iters = fixedIters;

  if (mode === "timeBudget") {
    iters = chooseItersForTimeBudget((probeIters) => measureUnary(fn, probeIters)[0], timeBudgetMs);
  }

  iters = clampU32(iters);
  if (mode === "timeBudget") {
    iters = Math.max(1, iters);
  }

  if (warmup) {
    fn(iters);
  }

  const [durationMs, checksum] = measureUnary(fn, iters);
  return mkCaseResult(name, durationMs, iters, checksum);
}

async function runBinaryCase(
  name: MicrobenchCaseName,
  fn: (bytes: number, iters: number) => unknown,
  bytes: number,
  mode: MicrobenchMode,
  warmup: boolean,
  timeBudgetMs: number,
  fixedIters: number,
): Promise<MicrobenchCaseResultV1> {
  let iters = fixedIters;

  if (mode === "timeBudget") {
    // Ensure any one-time allocation/initialization cost is paid before calibration.
    fn(bytes, 1);
    iters = chooseItersForTimeBudget(
      (probeIters) => measureBinary(fn, bytes, probeIters)[0],
      timeBudgetMs,
    );
  }

  iters = clampU32(iters);
  if (mode === "timeBudget") {
    iters = Math.max(1, iters);
  }

  if (warmup) {
    fn(bytes, iters);
  }

  const [durationMs, checksum] = measureBinary(fn, bytes, iters);
  return mkCaseResult(name, durationMs, iters, checksum, bytes);
}

export async function runMicrobenchSuite(
  opts?: MicrobenchSuiteOptions,
): Promise<MicrobenchSuiteResultV1> {
  await ensureMicrobenchWasm();

  const resolvedOpts = resolveOptions(opts);

  clearBenchmarks();

  const perfApi = window.aero?.perf;
  perfApi?.captureReset?.();
  perfApi?.captureStart?.();

  const startedTs = nowMs();

  const cases: MicrobenchSuiteResultV1["cases"] = {
    integer_alu: {} as MicrobenchCaseResultV1,
    branchy: {} as MicrobenchCaseResultV1,
    memcpy: {} as MicrobenchCaseResultV1,
    hash: {} as MicrobenchCaseResultV1,
  };

  cases.integer_alu = await runUnaryCase(
    "integer_alu",
    bench_integer_alu,
    resolvedOpts.mode,
    resolvedOpts.warmup,
    resolvedOpts.timeBudgetMs,
    resolvedOpts.integerAluIters,
  );

  cases.branchy = await runUnaryCase(
    "branchy",
    bench_branchy,
    resolvedOpts.mode,
    resolvedOpts.warmup,
    resolvedOpts.timeBudgetMs,
    resolvedOpts.branchyIters,
  );

  cases.memcpy = await runBinaryCase(
    "memcpy",
    bench_memcpy,
    resolvedOpts.memcpyBytes,
    resolvedOpts.mode,
    resolvedOpts.warmup,
    resolvedOpts.timeBudgetMs,
    resolvedOpts.memcpyIters,
  );

  cases.hash = await runBinaryCase(
    "hash",
    bench_hash,
    resolvedOpts.hashBytes,
    resolvedOpts.mode,
    resolvedOpts.warmup,
    resolvedOpts.timeBudgetMs,
    resolvedOpts.hashIters,
  );

  const finishedTs = nowMs();
  perfApi?.captureStop?.();

  const results: MicrobenchSuiteResultV1 = {
    schema: "aero-microbench-suite-v1",
    started_ts_ms: startedTs,
    finished_ts_ms: finishedTs,
    opts: resolvedOpts,
    cases,
  };

  setBenchmark("microbench", results);

  return results;
}
