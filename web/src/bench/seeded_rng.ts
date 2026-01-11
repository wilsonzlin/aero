export type RandomSource = () => number;

/**
 * Small, deterministic PRNG suitable for driving repeatable benchmark access patterns.
 *
 * Implementation: Mulberry32 (public domain).
 * - Deterministic across JS engines (uses 32-bit integer math)
 * - Fast enough for hot-loop random sampling
 */
export function createMulberry32(seed: number): RandomSource {
  let t = seed >>> 0;
  return () => {
    t = (t + 0x6d2b79f5) >>> 0;
    let x = t;
    x = Math.imul(x ^ (x >>> 15), x | 1);
    x ^= x + Math.imul(x ^ (x >>> 7), x | 61);
    return ((x ^ (x >>> 14)) >>> 0) / 4294967296;
  };
}

export function deriveSeed(seed: number, stream: number): number {
  // Mix two uint32 values (SplitMix-ish avalanche) so independent streams don't overlap.
  let x = (seed ^ Math.imul(stream >>> 0, 0x9e3779b1)) >>> 0;
  x ^= x >>> 16;
  x = Math.imul(x, 0x85ebca6b) >>> 0;
  x ^= x >>> 13;
  x = Math.imul(x, 0xc2b2ae35) >>> 0;
  x ^= x >>> 16;
  return x >>> 0;
}

export function createRandomSource(seed: number | undefined, stream: number): RandomSource {
  if (seed === undefined) return Math.random;
  return createMulberry32(deriveSeed(seed >>> 0, stream));
}

export function randomInt(maxExclusive: number, rand: RandomSource = Math.random): number {
  if (maxExclusive <= 1) return 0;
  return Math.floor(rand() * maxExclusive);
}

export function randomAlignedOffset(
  maxBytes: number,
  blockBytes: number,
  rand: RandomSource = Math.random,
): number {
  if (blockBytes <= 0 || maxBytes <= blockBytes) return 0;
  const blocks = Math.floor((maxBytes - blockBytes) / blockBytes) + 1;
  const idx = randomInt(blocks, rand);
  return idx * blockBytes;
}

