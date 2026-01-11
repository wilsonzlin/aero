/**
 * Lightweight JS fallback for the microbench WASM module.
 *
 * The canonical build uses `wasm-pack` to generate this module from
 * `crates/aero-microbench` into `web/src/wasm/aero_microbench/`.
 *
 * Repo-root builds (`npm run build`) don't currently run the WASM build step, but
 * we still want the app bundle (and CI perf harness) to build cleanly. This file
 * provides a small, deterministic implementation so the microbench suite remains
 * callable even when the WASM output is not present.
 *
 * When the real WASM module is generated, it will overwrite this file locally.
 */

/** @returns {Promise<void>} */
export default async function initMicrobench() {
  // No-op for JS fallback.
}

/**
 * Integer ALU benchmark.
 * @param {number} iters
 * @returns {number}
 */
export function bench_integer_alu(iters) {
  // Keep the arithmetic in the 32-bit range so results are stable.
  let acc = 0;
  let x = 0x12345678;
  for (let i = 0; i < iters; i += 1) {
    x = (x + 0x9e3779b9) | 0;
    x = (x ^ (x >>> 16)) | 0;
    acc = (acc + x) | 0;
  }
  return acc >>> 0;
}

/**
 * Branch-heavy benchmark.
 * @param {number} iters
 * @returns {number}
 */
export function bench_branchy(iters) {
  let acc = 0;
  let x = 0xdeadbeef;
  for (let i = 0; i < iters; i += 1) {
    x = (x * 1664525 + 1013904223) | 0;
    if (x & 1) acc = (acc + 3) | 0;
    else if (x & 2) acc = (acc ^ 0x55aa55aa) | 0;
    else acc = (acc - 7) | 0;
  }
  return acc >>> 0;
}

/**
 * "memcpy" benchmark.
 * @param {number} bytes
 * @param {number} iters
 * @returns {number}
 */
export function bench_memcpy(bytes, iters) {
  const size = Math.max(0, bytes | 0);
  const src = new Uint8Array(size);
  for (let i = 0; i < size; i += 1) src[i] = i & 0xff;
  const dst = new Uint8Array(size);

  let checksum = 0;
  for (let i = 0; i < iters; i += 1) {
    dst.set(src);
    // Touch a few bytes so the loop can't be elided completely.
    checksum = (checksum + dst[(i * 997) % (size || 1)]) | 0;
  }
  return checksum >>> 0;
}

/**
 * Simple FNV-1a hash benchmark.
 * @param {number} bytes
 * @param {number} iters
 * @returns {number}
 */
export function bench_hash(bytes, iters) {
  const size = Math.max(0, bytes | 0);
  const buf = new Uint8Array(size);
  for (let i = 0; i < size; i += 1) buf[i] = i & 0xff;

  let hash = 0x811c9dc5;
  for (let i = 0; i < iters; i += 1) {
    let h = hash;
    for (let j = 0; j < size; j += 1) {
      h ^= buf[j];
      h = Math.imul(h, 0x01000193);
    }
    hash = h >>> 0;
  }
  return hash >>> 0;
}

