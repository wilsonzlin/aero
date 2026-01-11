let initialized = false;

/**
 * Placeholder microbench implementation.
 *
 * The app expects a wasm-pack style module at
 * `web/src/wasm/aero_microbench/aero_microbench.js`. In environments where that
 * artifact hasn't been generated yet, we provide a small JS implementation so:
 * - TypeScript builds/typechecks don't fail due to a missing module.
 * - `window.aero.bench.runMicrobenchSuite()` still works in dev builds.
 */
export default async function initMicrobench(): Promise<void> {
  initialized = true;
}

function assertInitialized(): void {
  // Keep behavior permissive: if the caller forgets to await init, still run.
  if (!initialized) initialized = true;
}

export function bench_integer_alu(iters: number): number {
  assertInitialized();
  let x = 0x1234_5678 | 0;
  let y = 0x9e37_79b9 | 0;
  const n = Math.max(0, iters | 0);
  for (let i = 0; i < n; i += 1) {
    x = (x + y) | 0;
    x = (x ^ (x << 13)) | 0;
    x = (x + (x >>> 7)) | 0;
    y = (y + 0x7f4a_7c15) | 0;
  }
  return (x ^ y) >>> 0;
}

export function bench_branchy(iters: number): number {
  assertInitialized();
  let acc = 0 | 0;
  let x = 1 | 0;
  const n = Math.max(0, iters | 0);
  for (let i = 0; i < n; i += 1) {
    if ((x & 1) === 0) acc = (acc + x) | 0;
    else acc = (acc ^ x) | 0;
    // LCG for deterministic branch patterns.
    x = (Math.imul(x, 1103515245) + 12345) | 0;
  }
  return acc >>> 0;
}

export function bench_memcpy(bytes: number, iters: number): number {
  assertInitialized();
  const len = Math.max(1, bytes | 0);
  const src = new Uint8Array(len);
  const dst = new Uint8Array(len);
  for (let i = 0; i < len; i += 1) {
    src[i] = (Math.imul(i, 31) + 17) & 0xff;
  }

  let checksum = 0 | 0;
  const n = Math.max(0, iters | 0);
  for (let i = 0; i < n; i += 1) {
    dst.set(src);
    checksum = (checksum + dst[(i * 997) % len]) | 0;
  }
  return checksum >>> 0;
}

export function bench_hash(bytes: number, iters: number): number {
  assertInitialized();
  const len = Math.max(1, bytes | 0);
  const buf = new Uint8Array(len);
  for (let i = 0; i < len; i += 1) {
    buf[i] = (Math.imul(i, 13) ^ 0xa5) & 0xff;
  }

  const n = Math.max(0, iters | 0);
  let state = 0x811c_9dc5 | 0; // FNV-1a offset basis.
  for (let iter = 0; iter < n; iter += 1) {
    let h = state;
    for (let i = 0; i < len; i += 1) {
      h ^= buf[i];
      h = Math.imul(h, 0x0100_0193);
    }
    state ^= h;
  }
  return state >>> 0;
}

