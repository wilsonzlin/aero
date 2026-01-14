export const I32_MIN = -2147483648;
export const I32_MAX = 2147483647;

/**
 * Negate a signed 32-bit integer value with saturation.
 *
 * JavaScript numbers are unbounded floats, but many of Aero's hot paths ultimately convert values
 * into i32 (e.g. `Int32Array`-backed input batches and wasm-bindgen APIs). Negating `i32::MIN`
 * (`-2147483648`) cannot be represented in i32 and would wrap back to `i32::MIN` when coerced,
 * effectively turning the negation into a no-op.
 */
export function negateI32Saturating(v: number): number {
  // Callers are expected to pass an i32 (e.g. `x | 0`). We still keep the fast-path branch-only
  // implementation here to avoid additional coercions on hot paths.
  if (v === I32_MIN) return I32_MAX;
  return -v;
}

