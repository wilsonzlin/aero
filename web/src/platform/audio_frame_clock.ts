const NS_PER_SEC = 1_000_000_000;
const NS_PER_MS = 1_000_000;

/**
 * Deterministic audio frame scheduler driven by a monotonic nanosecond clock.
 *
 * Mirrors `crates/aero-audio/src/clock.rs`.
 *
 * This converts monotonic time deltas → audio frames without cumulative rounding drift by keeping
 * an explicit remainder accumulator (`fracFp`), expressed in a fixed-point form where
 * 1 second = 1_000_000_000 fraction units.
 */
export class AudioFrameClock {
  /**
   * Audio sample rate to generate/consume at (frames per second).
   *
   * Always an integer.
   */
  public readonly sampleRateHz: number;
  /**
   * Last time passed to `advanceTo`, in nanoseconds.
   */
  public lastTimeNs: number;
  /**
   * Fractional remainder accumulator.
   *
   * This is the remainder from dividing `deltaNs * sampleRateHz + fracFp` by 1_000_000_000.
   * It is therefore always in `[0, 1_000_000_000)`.
   */
  public fracFp: number;

  constructor(sampleRateHz: number, startTimeNs: number) {
    if (!Number.isFinite(sampleRateHz) || sampleRateHz <= 0) {
      throw new Error("sampleRateHz must be > 0");
    }
    // Keep the internal arithmetic integer-based (Rust uses `u32`).
    this.sampleRateHz = Math.floor(sampleRateHz);
    if (this.sampleRateHz <= 0) {
      throw new Error("sampleRateHz must be > 0");
    }

    if (!Number.isFinite(startTimeNs)) {
      throw new Error("startTimeNs must be a finite number");
    }
    this.lastTimeNs = Math.floor(startTimeNs);
    this.fracFp = 0;
  }

  /**
   * Advance the clock to `nowNs` and return the number of audio frames that elapsed since the
   * previous call.
   *
   * If `nowNs` is earlier than the last observed time, the delta is treated as 0 and the internal
   * time does not move backwards.
   */
  advanceTo(nowNs: number): number {
    if (!Number.isFinite(nowNs)) return 0;
    const now = Math.floor(nowNs);

    if (now <= this.lastTimeNs) {
      return 0;
    }

    const deltaNs = now - this.lastTimeNs;
    this.lastTimeNs = now;

    // Avoid overflowing the 53-bit integer mantissa in `deltaNs * sampleRateHz` for long deltas by
    // splitting into whole seconds + remainder nanoseconds:
    //   deltaNs = wholeSeconds * 1e9 + remNs
    //   frames = wholeSeconds * sampleRateHz + floor((fracFp + remNs * sampleRateHz) / 1e9)
    //
    // This is algebraically identical to the Rust implementation, while keeping intermediate
    // products bounded by `1e9 * sampleRateHz` (rather than `deltaNs * sampleRateHz`).
    const wholeSeconds = Math.floor(deltaNs / NS_PER_SEC);
    const remNs = deltaNs - wholeSeconds * NS_PER_SEC;

    const sr = this.sampleRateHz;

    // Clamp in the (pathological) case where frames can't be represented as a safe integer.
    const maxFrames = Number.MAX_SAFE_INTEGER;
    const maxWholeSeconds = Math.floor(maxFrames / sr);
    const framesFromSeconds = wholeSeconds > maxWholeSeconds ? maxFrames : wholeSeconds * sr;

    // `remNs < 1e9` so `remNs * sr` stays within ~1e14 for realistic sample rates.
    const totalRem = this.fracFp + remNs * sr;
    const framesFromRem = Math.floor(totalRem / NS_PER_SEC);
    let nextFrac = totalRem - framesFromRem * NS_PER_SEC;

    // In case of floating point rounding at extreme magnitudes, force the invariant.
    if (nextFrac >= NS_PER_SEC) {
      nextFrac %= NS_PER_SEC;
    } else if (nextFrac < 0) {
      nextFrac = ((nextFrac % NS_PER_SEC) + NS_PER_SEC) % NS_PER_SEC;
    }

    this.fracFp = nextFrac;

    const frames = framesFromSeconds + framesFromRem;
    return frames > maxFrames ? maxFrames : frames;
  }

  /**
   * Convenience helper for use with `performance.now()` / DOMHighResTimeStamp.
   *
   * The timestamp is converted to an integer nanosecond value via `floor(nowMs * 1e6)`.
   */
  advanceToMs(nowMs: number): number {
    return this.advanceTo(msToNs(nowMs));
  }
}

export function msToNs(nowMs: number): number {
  if (!Number.isFinite(nowMs)) return 0;
  return Math.floor(nowMs * NS_PER_MS);
}

