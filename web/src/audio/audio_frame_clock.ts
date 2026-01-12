const NS_PER_SEC = 1_000_000_000n;

function toBigIntNs(nowNs: number | bigint): bigint {
  if (typeof nowNs === "bigint") return nowNs;
  if (!Number.isFinite(nowNs)) throw new Error(`nowNs must be a finite number, got ${String(nowNs)}`);
  if (nowNs < 0) throw new Error(`nowNs must be >= 0, got ${nowNs}`);
  // Converting an imprecise `number` into a `bigint` would make the clock's output
  // non-deterministic. Force callers to use `bigint` if they need larger ranges.
  if (nowNs > Number.MAX_SAFE_INTEGER) {
    throw new Error(`nowNs is too large to represent precisely as a number; pass a bigint instead (${nowNs})`);
  }
  return BigInt(Math.floor(nowNs));
}

/**
 * Convert a `performance.now()` timestamp (milliseconds) into an integer nanosecond
 * timestamp suitable for {@link AudioFrameClock}.
 *
 * Note: `performance.now()` returns a floating-point value with
 * implementation-defined precision (often microseconds, not true nanoseconds).
 * We use `Math.floor` to avoid rounding up due to floating-point error.
 */
export function perfNowMsToNs(perfNowMs: number): bigint {
  if (!Number.isFinite(perfNowMs)) throw new Error(`perfNowMs must be finite, got ${String(perfNowMs)}`);
  if (perfNowMs < 0) throw new Error(`perfNowMs must be >= 0, got ${perfNowMs}`);

  // Fast path: if `perfNowMs` is already an integer millisecond timestamp
  // (e.g. `Date.now()`), convert without going through floating-point
  // multiplication so large epoch millisecond values remain exact.
  if (Number.isSafeInteger(perfNowMs)) {
    return BigInt(perfNowMs) * 1_000_000n;
  }

  // Common case: `performance.now()` is relative to the page/worker time origin,
  // so `perfNowMs * 1e6` stays below `Number.MAX_SAFE_INTEGER` for typical
  // sessions (<~104 days). Use a single multiply+floor to avoid introducing extra
  // rounding error from splitting integer/fractional components.
  const maxMsForSafeMul = Number.MAX_SAFE_INTEGER / 1e6;
  if (perfNowMs <= maxMsForSafeMul) {
    return BigInt(Math.floor(perfNowMs * 1e6));
  }

  // Fallback: extremely long-running sessions with sub-ms precision. Convert the
  // integer milliseconds exactly, then add the fractional microseconds.
  const msInt = Math.floor(perfNowMs);
  const fracMs = perfNowMs - msInt;
  const fracUs = Math.floor(fracMs * 1e6);
  return BigInt(msInt) * 1_000_000n + BigInt(Math.max(0, Math.min(999_999, fracUs)));
}

/**
 * Best-effort monotonic nanosecond "now" for browser/worker scheduling.
 *
 * Prefers `performance.now()` (relative monotonic clock). Falls back to `Date.now()`
 * with millisecond precision if `performance` is unavailable.
 */
export function performanceNowNs(): bigint {
  const perfNow = globalThis.performance?.now;
  if (typeof perfNow === "function") return perfNowMsToNs(perfNow.call(globalThis.performance));
  // `Date.now()` is epoch milliseconds (integer), so we can convert without going
  // through floating-point.
  return BigInt(Date.now()) * 1_000_000n;
}

/**
 * Deterministic time→audio-frame conversion without cumulative rounding drift.
 *
 * Mirrors Rust `crates/aero-audio/src/clock.rs`.
 */
export class AudioFrameClock {
  readonly sampleRateHz: number;
  lastTimeNs: bigint;
  /**
   * Remainder accumulator in the same fixed-point units as Rust `frac_fp`.
   *
   * This is the remainder from dividing `delta_ns * sampleRateHz + fracNsTimesRate`
   * by `1_000_000_000` (nanoseconds per second). Always `< 1_000_000_000`.
   */
  fracNsTimesRate: bigint;

  readonly #sampleRateHzBig: bigint;

  constructor(sampleRateHz: number, startTimeNs: number | bigint) {
    if (!Number.isFinite(sampleRateHz)) {
      throw new Error(`sampleRateHz must be finite, got ${String(sampleRateHz)}`);
    }
    const sr = Math.floor(sampleRateHz);
    if (sr <= 0) throw new Error(`sampleRateHz must be > 0, got ${sampleRateHz}`);

    this.sampleRateHz = sr;
    this.#sampleRateHzBig = BigInt(sr);
    this.lastTimeNs = toBigIntNs(startTimeNs);
    this.fracNsTimesRate = 0n;
  }

  /**
   * Advance the clock to `nowNs` and return the number of audio frames that
   * elapsed since the previous call.
   *
   * If `nowNs` is earlier than the last observed time, the delta is treated as 0
   * and the internal time does not move backwards.
   */
  advanceTo(nowNs: number | bigint): number {
    const now = toBigIntNs(nowNs);
    if (now <= this.lastTimeNs) return 0;

    const deltaNs = now - this.lastTimeNs;
    this.lastTimeNs = now;

    const total = this.fracNsTimesRate + deltaNs * this.#sampleRateHzBig;
    const frames = total / NS_PER_SEC;
    this.fracNsTimesRate = total % NS_PER_SEC;

    const max = BigInt(Number.MAX_SAFE_INTEGER);
    return frames > max ? Number.MAX_SAFE_INTEGER : Number(frames);
  }
}
