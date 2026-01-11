/**
 * Lightweight JS fallback for the `aero-microbench` wasm-pack output.
 *
 * The canonical build uses `wasm-pack` to generate this module from
 * `crates/aero-microbench` into `web/src/wasm/aero_microbench/`.
 *
 * The repo-root build/typecheck flows should not depend on having Rust/WASM
 * toolchains available, so this file provides a tiny deterministic
 * implementation that matches the API expected by `web/src/bench/microbench.ts`.
 *
 * When the real WASM module is generated, it will overwrite this file locally.
 */

function clampU32(value) {
  const n = Number.isFinite(value) ? Math.floor(value) : 0;
  if (n <= 0) return 0;
  if (n >= 0xffff_ffff) return 0xffff_ffff;
  return n >>> 0;
}

function clampBytes(bytes) {
  // Keep allocations bounded; this is just a placeholder module.
  return Math.min(clampU32(bytes), 64 * 1024);
}

function clampIters(iters) {
  // Cap work so calling code cannot accidentally lock up the page.
  return Math.min(clampU32(iters), 1_000_000);
}

export default async function initMicrobench() {
  // wasm-pack builds expose an async init. The JS fallback has no init work.
}

export function bench_integer_alu(iters) {
  const n = clampIters(iters);
  let acc = 0;
  for (let i = 0; i < n; i++) {
    acc = (acc + ((i ^ acc) + 0x9e37_79b9)) | 0;
    acc ^= acc >>> 16;
  }
  return acc >>> 0;
}

export function bench_branchy(iters) {
  const n = clampIters(iters);
  let acc = 0;
  for (let i = 0; i < n; i++) {
    if ((i & 7) === 0) acc = (acc + i) | 0;
    else if ((i & 7) === 1) acc = (acc - i) | 0;
    else if ((i & 7) === 2) acc ^= i;
    else if ((i & 7) === 3) acc = (acc * 3) | 0;
    else if ((i & 7) === 4) acc = (acc * 5 + 1) | 0;
    else acc = (acc + (i ^ (acc >>> 1))) | 0;
  }
  return acc >>> 0;
}

export function bench_memcpy(bytes, iters) {
  const len = clampBytes(bytes);
  const n = Math.min(clampU32(iters), 10_000);
  const src = new Uint8Array(len);
  const dst = new Uint8Array(len);
  for (let i = 0; i < len; i++) src[i] = (i * 31 + 7) & 0xff;
  for (let i = 0; i < n; i++) {
    dst.set(src);
    // Mix a tiny checksum so work isn't optimized away.
    src[0] ^= dst[(i * 17) % len];
  }
  return (src[0] | (src[len - 1] << 8) | (dst[0] << 16) | (dst[len - 1] << 24)) >>> 0;
}

export function bench_hash(bytes, iters) {
  const len = clampBytes(bytes);
  const n = Math.min(clampU32(iters), 10_000);
  const data = new Uint8Array(len);
  for (let i = 0; i < len; i++) data[i] = (i * 131 + 13) & 0xff;

  let hash = 0x811c9dc5;
  for (let iter = 0; iter < n; iter++) {
    // FNV-1a over a bounded slice.
    for (let i = 0; i < len; i++) {
      hash ^= data[i];
      hash = Math.imul(hash, 0x0100_0193) >>> 0;
    }
    data[iter % len] ^= hash & 0xff;
  }
  return hash >>> 0;
}
