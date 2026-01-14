/**
 * Unsigned `u32` subtraction with wraparound.
 *
 * `performance.now()`-derived microsecond timestamps are often stored as wrapping u32 values
 * for cheap transport and compact shared memory. This helper computes `now - then` with
 * 32-bit wrap semantics.
 */
export function u32Delta(now: number, then: number): number {
  return ((now >>> 0) - (then >>> 0)) >>> 0;
}

