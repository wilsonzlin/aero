export function asU64(v: bigint): bigint {
  return BigInt.asUintN(64, v);
}

export function asI64(v: bigint): bigint {
  return BigInt.asIntN(64, v);
}

/**
 * Convert a wasm i64/u64 to a JS `number` for use as an address/offset, asserting it is in the
 * `< 4 GiB` range (safe for `number` indexing).
 */
export function u64ToNumber(v: bigint): number {
  const u = asU64(v);
  // 2^32
  if (u >= 0x1_0000_0000n) {
    throw new RangeError(`u64ToNumber: value out of range (< 4 GiB required): ${u.toString()}`);
  }
  return Number(u);
}

