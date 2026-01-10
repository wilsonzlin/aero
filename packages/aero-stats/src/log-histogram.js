function intLog2(x) {
  if (x < 1) return 0;
  if (x < 0x1_0000_0000) return 31 - Math.clz32(x);
  return Math.floor(Math.log2(x));
}

export class LogHistogram {
  #subBucketCount;
  #counts;
  #maxExp;
  #totalCount = 0;
  #min = Infinity;
  #max = -Infinity;
  #minExpUsed = Infinity;
  #maxExpUsed = -Infinity;
  #touched = [];

  constructor({ subBucketCount = 1024, maxExponent = 30 } = {}) {
    if (!Number.isInteger(subBucketCount) || subBucketCount <= 0) {
      throw new TypeError(
        `LogHistogram: subBucketCount must be a positive integer, got ${subBucketCount}`,
      );
    }
    if (!Number.isInteger(maxExponent) || maxExponent < 0) {
      throw new TypeError(
        `LogHistogram: maxExponent must be a non-negative integer, got ${maxExponent}`,
      );
    }

    this.#subBucketCount = subBucketCount;
    this.#maxExp = maxExponent;
    this.#counts = new Uint32Array((maxExponent + 1) * subBucketCount);
  }

  get subBucketCount() {
    return this.#subBucketCount;
  }

  get totalCount() {
    return this.#totalCount;
  }

  get min() {
    return this.#totalCount === 0 ? Number.NaN : this.#min;
  }

  get max() {
    return this.#totalCount === 0 ? Number.NaN : this.#max;
  }

  record(value, count = 1) {
    if (!Number.isFinite(value)) {
      throw new TypeError(`LogHistogram.record: value must be finite, got ${value}`);
    }
    if (!Number.isInteger(count) || count <= 0) {
      throw new TypeError(
        `LogHistogram.record: count must be a positive integer, got ${count}`,
      );
    }

    const v = value < 1 ? 1 : value;
    const exp = intLog2(v);
    this.#ensureExponent(exp);

    const base = 2 ** exp;
    const offset = v - base;
    const bucket = Math.min(
      this.#subBucketCount - 1,
      Math.floor((offset * this.#subBucketCount) / base),
    );

    const idx = exp * this.#subBucketCount + bucket;
    if (this.#counts[idx] === 0) this.#touched.push(idx);
    this.#counts[idx] += count;
    this.#totalCount += count;
    this.#min = Math.min(this.#min, v);
    this.#max = Math.max(this.#max, v);
    this.#minExpUsed = Math.min(this.#minExpUsed, exp);
    this.#maxExpUsed = Math.max(this.#maxExpUsed, exp);
  }

  merge(other) {
    if (!(other instanceof LogHistogram)) {
      throw new TypeError('LogHistogram.merge: other must be a LogHistogram');
    }
    if (other.#totalCount === 0) return;
    if (this.#subBucketCount !== other.#subBucketCount) {
      throw new TypeError(
        `LogHistogram.merge: incompatible subBucketCount (${this.#subBucketCount} vs ${other.#subBucketCount})`,
      );
    }

    this.#ensureExponent(other.#maxExpUsed);

    const startExp = Math.max(0, other.#minExpUsed);
    const endExp = Math.max(startExp, other.#maxExpUsed);
    const start = startExp * this.#subBucketCount;
    const end = (endExp + 1) * this.#subBucketCount;
    for (let i = start; i < end; i += 1) {
      const c = other.#counts[i];
      if (c !== 0) {
        if (this.#counts[i] === 0) this.#touched.push(i);
        this.#counts[i] += c;
      }
    }

    this.#totalCount += other.#totalCount;
    this.#min = Math.min(this.#min, other.#min);
    this.#max = Math.max(this.#max, other.#max);
    this.#minExpUsed = Math.min(this.#minExpUsed, other.#minExpUsed);
    this.#maxExpUsed = Math.max(this.#maxExpUsed, other.#maxExpUsed);
  }

  quantile(q) {
    if (!Number.isFinite(q)) {
      throw new TypeError(`LogHistogram.quantile: q must be finite, got ${q}`);
    }

    if (this.#totalCount === 0) return Number.NaN;
    if (q <= 0) return this.#min;
    if (q >= 1) return this.#max;

    const targetRank = Math.ceil(q * this.#totalCount);
    let cumulative = 0;

    const startExp = Math.max(0, this.#minExpUsed);
    const endExp = Math.max(startExp, this.#maxExpUsed);

    for (let exp = startExp; exp <= endExp; exp += 1) {
      const base = 2 ** exp;
      const bucketWidth = base / this.#subBucketCount;
      const baseIndex = exp * this.#subBucketCount;

      for (let bucket = 0; bucket < this.#subBucketCount; bucket += 1) {
        const c = this.#counts[baseIndex + bucket];
        if (c === 0) continue;

        const next = cumulative + c;
        if (next >= targetRank) {
          const bucketLo = base + bucket * bucketWidth;
          const bucketHi = bucketLo + bucketWidth;
          const inside = (targetRank - cumulative) / c;
          const approx = bucketLo + (bucketHi - bucketLo) * inside;
          return Math.min(this.#max, Math.max(this.#min, approx));
        }
        cumulative = next;
      }
    }

    return this.#max;
  }

  clear() {
    for (const idx of this.#touched) {
      this.#counts[idx] = 0;
    }
    this.#touched.length = 0;
    this.#totalCount = 0;
    this.#min = Infinity;
    this.#max = -Infinity;
    this.#minExpUsed = Infinity;
    this.#maxExpUsed = -Infinity;
  }

  toJSON() {
    const sparseCounts = [];
    const startExp = Number.isFinite(this.#minExpUsed) ? this.#minExpUsed : 0;
    const endExp = Number.isFinite(this.#maxExpUsed) ? this.#maxExpUsed : -1;

    for (let exp = startExp; exp <= endExp; exp += 1) {
      const baseIndex = exp * this.#subBucketCount;
      for (let bucket = 0; bucket < this.#subBucketCount; bucket += 1) {
        const idx = baseIndex + bucket;
        const c = this.#counts[idx];
        if (c !== 0) sparseCounts.push([idx, c]);
      }
    }

    return {
      type: 'LogHistogram',
      subBucketCount: this.#subBucketCount,
      maxExponent: this.#maxExp,
      totalCount: this.#totalCount,
      min: this.#min,
      max: this.#max,
      sparseCounts,
    };
  }

  static fromJSON(data) {
    if (!data || data.type !== 'LogHistogram') {
      throw new TypeError('LogHistogram.fromJSON: invalid payload');
    }
    const hist = new LogHistogram({
      subBucketCount: data.subBucketCount,
      maxExponent: data.maxExponent,
    });
    hist.#totalCount = data.totalCount;
    hist.#min = data.min;
    hist.#max = data.max;

    for (const [idx, c] of data.sparseCounts) {
      hist.#counts[idx] = c;
      if (c !== 0) hist.#touched.push(idx);
      const exp = Math.floor(idx / hist.#subBucketCount);
      hist.#minExpUsed = Math.min(hist.#minExpUsed, exp);
      hist.#maxExpUsed = Math.max(hist.#maxExpUsed, exp);
    }

    if (hist.#totalCount === 0) {
      hist.#minExpUsed = Infinity;
      hist.#maxExpUsed = -Infinity;
    }

    return hist;
  }

  #ensureExponent(exp) {
    if (exp <= this.#maxExp) return;

    const newMax = exp;
    const next = new Uint32Array((newMax + 1) * this.#subBucketCount);
    next.set(this.#counts);
    this.#counts = next;
    this.#maxExp = newMax;
  }
}
