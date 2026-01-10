export class RunningStats {
  #count = 0;
  #mean = 0;
  #m2 = 0;
  #min = Infinity;
  #max = -Infinity;
  #sum = 0;
  #sumCompensation = 0;

  clear() {
    this.#count = 0;
    this.#mean = 0;
    this.#m2 = 0;
    this.#min = Infinity;
    this.#max = -Infinity;
    this.#sum = 0;
    this.#sumCompensation = 0;
  }

  get count() {
    return this.#count;
  }

  get min() {
    return this.#count === 0 ? Number.NaN : this.#min;
  }

  get max() {
    return this.#count === 0 ? Number.NaN : this.#max;
  }

  get sum() {
    return this.#sum;
  }

  get mean() {
    return this.#count === 0 ? Number.NaN : this.#mean;
  }

  get variancePopulation() {
    if (this.#count === 0) return Number.NaN;
    if (this.#count === 1) return 0;
    return this.#m2 / this.#count;
  }

  get varianceSample() {
    if (this.#count < 2) return Number.NaN;
    return this.#m2 / (this.#count - 1);
  }

  get stdevPopulation() {
    const v = this.variancePopulation;
    return Number.isNaN(v) ? Number.NaN : Math.sqrt(v);
  }

  get stdevSample() {
    const v = this.varianceSample;
    return Number.isNaN(v) ? Number.NaN : Math.sqrt(v);
  }

  get coefficientOfVariation() {
    const mean = this.mean;
    if (!Number.isFinite(mean) || mean === 0) return Number.NaN;
    return this.stdevPopulation / mean;
  }

  push(value) {
    if (!Number.isFinite(value)) {
      throw new TypeError(`RunningStats.push: value must be finite, got ${value}`);
    }

    this.#kahanAdd(value);

    this.#count += 1;
    this.#min = Math.min(this.#min, value);
    this.#max = Math.max(this.#max, value);

    const delta = value - this.#mean;
    this.#mean += delta / this.#count;
    const delta2 = value - this.#mean;
    this.#m2 += delta * delta2;
  }

  merge(other) {
    if (!(other instanceof RunningStats)) {
      throw new TypeError('RunningStats.merge: other must be a RunningStats');
    }

    if (other.#count === 0) return;
    if (this.#count === 0) {
      this.#count = other.#count;
      this.#mean = other.#mean;
      this.#m2 = other.#m2;
      this.#min = other.#min;
      this.#max = other.#max;
      this.#sum = other.#sum;
      this.#sumCompensation = other.#sumCompensation;
      return;
    }

    const totalCount = this.#count + other.#count;
    const delta = other.#mean - this.#mean;

    const mean = this.#mean + (delta * other.#count) / totalCount;
    const m2 =
      this.#m2 +
      other.#m2 +
      (delta * delta * this.#count * other.#count) / totalCount;

    this.#mean = mean;
    this.#m2 = m2;
    this.#count = totalCount;
    this.#min = Math.min(this.#min, other.#min);
    this.#max = Math.max(this.#max, other.#max);
    this.#kahanAdd(other.#sum);
  }

  toJSON() {
    return {
      type: 'RunningStats',
      count: this.#count,
      mean: this.#mean,
      m2: this.#m2,
      min: this.#min,
      max: this.#max,
      sum: this.#sum,
    };
  }

  static fromJSON(data) {
    if (!data || data.type !== 'RunningStats') {
      throw new TypeError('RunningStats.fromJSON: invalid payload');
    }

    const stats = new RunningStats();
    stats.#count = data.count;
    stats.#mean = data.mean;
    stats.#m2 = data.m2;
    stats.#min = data.min;
    stats.#max = data.max;
    stats.#sum = data.sum;
    stats.#sumCompensation = 0;
    return stats;
  }

  #kahanAdd(value) {
    const y = value - this.#sumCompensation;
    const t = this.#sum + y;
    this.#sumCompensation = t - this.#sum - y;
    this.#sum = t;
  }
}
