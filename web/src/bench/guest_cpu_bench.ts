import { initWasmForContext } from "../runtime/wasm_context";
import type { GuestCpuBenchOpts, GuestCpuBenchRun, GuestCpuBenchVariant, GuestCpuMode } from "./guest_cpu_types";

const DEFAULT_ITERS_PER_RUN = 10_000;
const DEFAULT_WARMUP_RUNS = 3;
const DEFAULT_SECONDS = 0.25;

function nowMs(): number {
  return typeof performance?.now === "function" ? performance.now() : Date.now();
}

function requirePositiveFiniteSeconds(seconds: number): number {
  if (!Number.isFinite(seconds)) {
    throw new Error(`Guest CPU benchmark: "seconds" must be a finite number. Got: ${String(seconds)}`);
  }
  if (seconds <= 0) {
    throw new Error(`Guest CPU benchmark: "seconds" must be > 0. Got: ${String(seconds)}`);
  }
  return seconds;
}

function requirePositiveU32Iters(iters: number): number {
  if (!Number.isFinite(iters)) {
    throw new Error(`Guest CPU benchmark: "iters" must be a finite number. Got: ${String(iters)}`);
  }
  if (!Number.isInteger(iters)) {
    throw new Error(`Guest CPU benchmark: "iters" must be an integer. Got: ${String(iters)}`);
  }
  if (iters <= 0) {
    throw new Error(`Guest CPU benchmark: "iters" must be > 0. Got: ${String(iters)}`);
  }
  if (iters > 0xffff_ffff) {
    throw new Error(`Guest CPU benchmark: "iters" must be <= 4294967295 (u32). Got: ${String(iters)}`);
  }
  return iters;
}

function readNumberField(obj: unknown, key: string): number | undefined {
  if (!obj || typeof obj !== "object") return undefined;
  const record = obj as Record<string, unknown>;
  const value = record[key];
  if (typeof value === "number") return value;
  if (typeof value === "function") {
    try {
      // wasm-bindgen getters may appear as methods in some builds.
      const out = (value as (...args: unknown[]) => unknown).call(obj);
      return typeof out === "number" ? out : undefined;
    } catch {
      return undefined;
    }
  }
  return undefined;
}

function readBigIntField(obj: unknown, key: string): bigint | undefined {
  if (!obj || typeof obj !== "object") return undefined;
  const record = obj as Record<string, unknown>;
  const value = record[key];
  if (typeof value === "bigint") return value;
  if (typeof value === "function") {
    try {
      // wasm-bindgen getters may appear as methods in some builds.
      const out = (value as (...args: unknown[]) => unknown).call(obj);
      return typeof out === "bigint" ? out : undefined;
    } catch {
      return undefined;
    }
  }
  return undefined;
}

function describeValue(value: unknown): string {
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

function splitU64(v: bigint): { hi: number; lo: number } {
  const hi = Number((v >> 32n) & 0xffff_ffffn);
  const lo = Number(v & 0xffff_ffffn);
  return { hi, lo };
}

function readU32Field(obj: unknown, key: string): number | undefined {
  const n = readNumberField(obj, key);
  if (n === undefined) return undefined;
  return n >>> 0;
}

function formatHexFixed(value: bigint, digits: number): string {
  const hex = value.toString(16);
  return `0x${hex.padStart(digits, "0")}`;
}

function formatChecksum(bitness: 32 | 64, hi: number, lo: number): string {
  if (bitness === 32) {
    return formatHexFixed(BigInt(lo >>> 0), 8);
  }
  const v = (BigInt(hi >>> 0) << 32n) | BigInt(lo >>> 0);
  return formatHexFixed(v, 16);
}

function u64ToNumber(hi: number, lo: number): number {
  // NOTE: This assumes the result is within Number.MAX_SAFE_INTEGER for the
  // benchmark configuration used (short time budgets).
  return (hi >>> 0) * 2 ** 32 + (lo >>> 0);
}

function mean(values: number[]): number {
  if (values.length === 0) return 0;
  let sum = 0;
  for (const v of values) sum += v;
  return sum / values.length;
}

function stddevPopulation(values: number[], meanValue: number): number {
  if (values.length === 0) return 0;
  let acc = 0;
  for (const v of values) {
    const d = v - meanValue;
    acc += d * d;
  }
  return Math.sqrt(acc / values.length);
}

function min(values: number[]): number {
  if (values.length === 0) return 0;
  let m = values[0]!;
  for (let i = 1; i < values.length; i++) m = Math.min(m, values[i]!);
  return m;
}

function max(values: number[]): number {
  if (values.length === 0) return 0;
  let m = values[0]!;
  for (let i = 1; i < values.length; i++) m = Math.max(m, values[i]!);
  return m;
}

type ParsedPayloadInfo = {
  bitness: 32 | 64;
  expected_checksum_hi: number;
  expected_checksum_lo: number;
};

function parsePayloadInfo(info: unknown): ParsedPayloadInfo {
  const bitnessRaw =
    readNumberField(info, "bitness") ??
    readNumberField(info, "bits") ??
    readNumberField(info, "mode") ??
    readNumberField(info, "payload_bitness");
  const bitness = bitnessRaw === 32 || bitnessRaw === 64 ? bitnessRaw : undefined;
  if (!bitness) {
    throw new Error(
      `GuestCpuBenchHarness.payload_info returned invalid bitness (${String(bitnessRaw)}). Expected 32 or 64.`,
    );
  }

  let expectedHi =
    readU32Field(info, "expected_checksum_hi") ??
    readU32Field(info, "expected_hi") ??
    readU32Field(info, "checksum_hi") ??
    readU32Field(info, "hi");
  let expectedLo =
    readU32Field(info, "expected_checksum_lo") ??
    readU32Field(info, "expected_lo") ??
    readU32Field(info, "checksum_lo") ??
    readU32Field(info, "lo");

  if (expectedHi === undefined || expectedLo === undefined) {
    const combined = readBigIntField(info, "expected_checksum") ?? readBigIntField(info, "checksum");
    if (combined !== undefined) {
      const parts = splitU64(combined);
      expectedHi = parts.hi;
      expectedLo = parts.lo;
    }
  }

  if (expectedHi === undefined || expectedLo === undefined) {
    throw new Error(
      `GuestCpuBenchHarness.payload_info returned missing expected checksum fields. Got: ${describeValue(info)}`,
    );
  }

  if (bitness === 32) {
    expectedHi = 0;
  }

  return {
    bitness,
    expected_checksum_hi: expectedHi,
    expected_checksum_lo: expectedLo,
  };
}

type ParsedRunResult = {
  checksum_hi: number;
  checksum_lo: number;
  retired_instructions_hi: number;
  retired_instructions_lo: number;
};

function parseRunResult(result: unknown): ParsedRunResult {
  let checksumHi =
    readU32Field(result, "checksum_hi") ??
    readU32Field(result, "observed_checksum_hi") ??
    readU32Field(result, "hi");
  let checksumLo =
    readU32Field(result, "checksum_lo") ??
    readU32Field(result, "observed_checksum_lo") ??
    readU32Field(result, "lo");

  if (checksumHi === undefined || checksumLo === undefined) {
    const combined = readBigIntField(result, "checksum");
    if (combined !== undefined) {
      const parts = splitU64(combined);
      checksumHi = parts.hi;
      checksumLo = parts.lo;
    }
  }

  let instHi =
    readU32Field(result, "retired_instructions_hi") ??
    readU32Field(result, "retired_hi") ??
    readU32Field(result, "instructions_hi") ??
    readU32Field(result, "retired_hi") ??
    readU32Field(result, "insts_hi");
  let instLo =
    readU32Field(result, "retired_instructions_lo") ??
    readU32Field(result, "retired_lo") ??
    readU32Field(result, "instructions_lo") ??
    readU32Field(result, "retired_lo") ??
    readU32Field(result, "insts_lo");

  if (instHi === undefined || instLo === undefined) {
    const combined = readBigIntField(result, "retired_instructions") ?? readBigIntField(result, "instructions");
    if (combined !== undefined) {
      const parts = splitU64(combined);
      instHi = parts.hi;
      instLo = parts.lo;
    }
  }

  if (checksumHi === undefined || checksumLo === undefined || instHi === undefined || instLo === undefined) {
    throw new Error(
      `GuestCpuBenchHarness.run_payload_once returned unexpected shape. Got: ${describeValue(result)}`,
    );
  }

  return {
    checksum_hi: checksumHi,
    checksum_lo: checksumLo,
    retired_instructions_hi: instHi,
    retired_instructions_lo: instLo,
  };
}

function checkChecksumOrThrow(params: {
  variant: GuestCpuBenchVariant;
  mode: GuestCpuMode;
  expected: string;
  observed: string;
}): void {
  if (params.expected === params.observed) return;
  throw new Error(
    [
      `Guest CPU benchmark checksum mismatch (${params.variant}, mode=${params.mode}).`,
      `Expected: ${params.expected}`,
      `Observed: ${params.observed}`,
      "Hint: likely an emulator correctness regression (fast-but-wrong).",
    ].join("\n"),
  );
}

function checkDeterminismOrThrow(params: {
  variant: GuestCpuBenchVariant;
  mode: GuestCpuMode;
  expected: string;
  observed: string;
}): void {
  if (params.expected === params.observed) return;
  throw new Error(
    [
      `Guest CPU benchmark determinism check failed (${params.variant}, mode=${params.mode}).`,
      `Expected checksum (reference run): ${params.expected}`,
      `Observed checksum (measured run): ${params.observed}`,
      "Hint: the payload is expected to be deterministic; this usually means guest-visible state was not fully reset between runs.",
    ].join("\n"),
  );
}

export async function runGuestCpuBench(opts: GuestCpuBenchOpts): Promise<GuestCpuBenchRun> {
  if (opts.seconds !== undefined && opts.iters !== undefined) {
    throw new Error('Guest CPU benchmark: "seconds" and "iters" are mutually exclusive; specify only one.');
  }

  // Validate option values early (even if the requested `mode` is not implemented),
  // so callers get consistent error messages for malformed inputs.
  const secondsValidated = opts.seconds !== undefined ? requirePositiveFiniteSeconds(opts.seconds) : undefined;
  const itersValidated = opts.iters !== undefined ? requirePositiveU32Iters(opts.iters) : undefined;

  if (opts.mode !== "interpreter") {
    throw new Error(
      `Guest CPU benchmark mode "${opts.mode}" is not implemented yet. Only "interpreter" is supported.`,
    );
  }

  const usingIters = itersValidated !== undefined;
  const secondsBudget = secondsValidated ?? (usingIters ? undefined : DEFAULT_SECONDS);

  const itersPerRun = usingIters ? itersValidated! : DEFAULT_ITERS_PER_RUN;
  const warmupRuns = secondsBudget !== undefined ? DEFAULT_WARMUP_RUNS : 0;

  const { api } = await initWasmForContext();
  if (!api.GuestCpuBenchHarness) {
    throw new Error(
      [
        "WASM GuestCpuBenchHarness is not available in this build.",
        "",
        "This usually means the generated wasm-bindgen package is out of date.",
        "Rebuild WASM with (from the repo root):",
        "  npm run wasm:build",
      ].join("\n"),
    );
  }

  const h = new api.GuestCpuBenchHarness();
  try {
    const payloadInfo = parsePayloadInfo(h.payload_info(opts.variant));
    const canonicalExpectedChecksum = formatChecksum(
      payloadInfo.bitness,
      payloadInfo.expected_checksum_hi,
      payloadInfo.expected_checksum_lo,
    );

    const runOnce = (): { seconds: number; insts: number; checksum: string } => {
      const t0 = nowMs();
      const result = h.run_payload_once(opts.variant, itersPerRun);
      const t1 = nowMs();

      const parsed = parseRunResult(result);
      const checksum = formatChecksum(payloadInfo.bitness, parsed.checksum_hi, parsed.checksum_lo);
      const insts = u64ToNumber(parsed.retired_instructions_hi, parsed.retired_instructions_lo);
      const seconds = (t1 - t0) / 1000;

      return { seconds, insts, checksum };
    };

    const runMips: number[] = [];
    let expectedChecksum: string;
    let observedChecksum: string;
    let totalInstructions = 0;
    let totalSeconds = 0;
    let measuredRuns = 0;

    if (secondsBudget !== undefined) {
      expectedChecksum = canonicalExpectedChecksum;
      observedChecksum = expectedChecksum;
      // Ensure at least one measured run, even if `secondsBudget` is very small.
      for (let i = 0; i < warmupRuns; i++) {
        const { checksum } = runOnce();
        checkChecksumOrThrow({
          variant: opts.variant,
          mode: opts.mode,
          expected: expectedChecksum,
          observed: checksum,
        });
      }

      while (measuredRuns < 1 || totalSeconds < secondsBudget) {
        const { seconds, insts, checksum } = runOnce();
        checkChecksumOrThrow({ variant: opts.variant, mode: opts.mode, expected: expectedChecksum, observed: checksum });
        observedChecksum = checksum;
        measuredRuns++;
        totalInstructions += insts;
        totalSeconds += seconds;

        const safeSeconds = seconds > 0 ? seconds : 1e-9;
        runMips.push((insts / safeSeconds) / 1e6);
      }
    } else {
      const reference = runOnce();
      if (itersPerRun === DEFAULT_ITERS_PER_RUN) {
        checkChecksumOrThrow({
          variant: opts.variant,
          mode: opts.mode,
          expected: canonicalExpectedChecksum,
          observed: reference.checksum,
        });
      }

      const measured = runOnce();
      if (itersPerRun === DEFAULT_ITERS_PER_RUN) {
        checkChecksumOrThrow({
          variant: opts.variant,
          mode: opts.mode,
          expected: canonicalExpectedChecksum,
          observed: measured.checksum,
        });
      }

      checkDeterminismOrThrow({
        variant: opts.variant,
        mode: opts.mode,
        expected: reference.checksum,
        observed: measured.checksum,
      });

      expectedChecksum = reference.checksum;
      observedChecksum = measured.checksum;
      measuredRuns = 1;
      totalInstructions = measured.insts;
      totalSeconds = measured.seconds;

      const safeSeconds = measured.seconds > 0 ? measured.seconds : 1e-9;
      runMips.push((measured.insts / safeSeconds) / 1e6);
    }

    const safeTotalSeconds = totalSeconds > 0 ? totalSeconds : 1e-9;
    const ips = totalInstructions / safeTotalSeconds;
    const mips = ips / 1e6;

    const mipsMean = mean(runMips);
    const mipsStddev = stddevPopulation(runMips, mipsMean);
    const mipsMin = min(runMips);
    const mipsMax = max(runMips);

    return {
      variant: opts.variant,
      mode: opts.mode,
      iters_per_run: itersPerRun,
      warmup_runs: warmupRuns,
      measured_runs: measuredRuns,
      expected_checksum: expectedChecksum,
      observed_checksum: observedChecksum,
      total_instructions: totalInstructions,
      total_seconds: totalSeconds,
      ips,
      mips,
      run_mips: runMips,
      mips_mean: mipsMean,
      mips_stddev: mipsStddev,
      mips_min: mipsMin,
      mips_max: mipsMax,
    };
  } finally {
    h.free();
  }
}
