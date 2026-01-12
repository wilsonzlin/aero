/**
 * IRQ refcount helper used by the browser worker runtime.
 *
 * Aero's worker-to-worker IRQ transport is *level* based, but multiple devices
 * may share a single IRQ line (legacy PIC / PCI INTx). To model "wire-OR"
 * semantics safely, each IRQ line is tracked as a small unsigned refcount:
 *
 * - `raise`  => increment the refcount
 * - `lower`  => decrement the refcount
 * - line is considered asserted when `refcount > 0`
 *
 * The implementation clamps to the range `[0, 0xffff]` to avoid silent typed
 * array wraparound.
 */

export const IRQ_LINE_COUNT = 256;

/**
 * Max value for the per-line refcount.
 *
 * We store refcounts in a `Uint16Array` for compactness. If code increments past
 * 0xffff, the typed array would wrap to 0 and silently corrupt IRQ state. Treat
 * that as a (dev-time) warning and saturate instead.
 */
export const IRQ_REFCOUNT_MAX = 0xffff;

// Bitflags returned by `applyIrqRefCountChange`.
export const IRQ_REFCOUNT_ASSERT = 1 << 0; // 0 -> 1 transition
export const IRQ_REFCOUNT_DEASSERT = 1 << 1; // 1 -> 0 transition
export const IRQ_REFCOUNT_UNDERFLOW = 1 << 2; // lower when already 0
export const IRQ_REFCOUNT_SATURATED = 1 << 3; // raise when already 0xffff

/**
 * Apply a single refcounted level change to an IRQ line.
 *
 * `level=true` corresponds to `raiseIrq` / `irqRaise` (assert).
 * `level=false` corresponds to `lowerIrq` / `irqLower` (deassert).
 *
 * Returns a bitmask of `IRQ_REFCOUNT_*` flags describing what happened.
 */
export function applyIrqRefCountChange(refCounts: Uint16Array, idx: number, level: boolean): number {
  const prev = refCounts[idx] ?? 0;

  if (level) {
    if (prev === IRQ_REFCOUNT_MAX) return IRQ_REFCOUNT_SATURATED;
    const next = prev + 1;
    refCounts[idx] = next;
    return prev === 0 ? IRQ_REFCOUNT_ASSERT : 0;
  }

  if (prev === 0) return IRQ_REFCOUNT_UNDERFLOW;
  const next = prev - 1;
  refCounts[idx] = next;
  return next === 0 ? IRQ_REFCOUNT_DEASSERT : 0;
}

