/**
 * Space-Saving / Metwally et al. heavy-hitter approximation.
 *
 * Maintains at most `capacity` counters while processing an unbounded stream.
 * Each call to `observe(key, weight)` is O(1) for known keys and O(capacity)
 * only when we need to replace the current minimum counter.
 *
 * This is intended for "hot path" telemetry where:
 *  - we cannot retain an unbounded per-PC hash map, and
 *  - we can tolerate approximate counts for long tails.
 *
 * References:
 *  - "Efficient Computation of Frequent and Top-k Elements in Data Streams"
 *    (Metwally, Agrawal, Abbadi) — a.k.a Space-Saving.
 */

const MAX_COUNT = Number.MAX_SAFE_INTEGER;

/** @param {number} a @param {number} b */
function addSaturating(a, b) {
  const sum = a + b;
  return sum >= MAX_COUNT ? MAX_COUNT : sum;
}

/**
 * @template TKey
 * @typedef {Object} SpaceSavingEntry
 * @property {TKey} key
 * @property {number} count Approximate count (may over-estimate by <= error).
 * @property {number} error Upper bound on over-count error.
 */

/**
 * @template TKey
 */
export class SpaceSavingTopK {
  /**
   * @param {number} capacity
   */
  constructor(capacity) {
    if (!Number.isInteger(capacity) || capacity <= 0) {
      throw new Error(`SpaceSavingTopK capacity must be a positive integer, got ${capacity}`);
    }
    this._capacity = capacity;
    /** @type {Array<SpaceSavingEntry<TKey>>} */
    this._entries = [];
    /** @type {Map<TKey, number>} */
    this._indexByKey = new Map();
    this._minIndex = -1;
  }

  /** @returns {number} */
  get capacity() {
    return this._capacity;
  }

  /** @returns {number} */
  get size() {
    return this._entries.length;
  }

  /** @returns {ReadonlyArray<SpaceSavingEntry<TKey>>} */
  get entries() {
    return this._entries;
  }

  /**
   * @param {TKey} key
   * @returns {SpaceSavingEntry<TKey> | undefined}
   */
  get(key) {
    const idx = this._indexByKey.get(key);
    if (idx === undefined) return undefined;
    return this._entries[idx];
  }

  /**
   * @param {TKey} key
   * @param {number} [weight=1]
   * @returns {{event: 'increment' | 'insert' | 'replace', replacedKey?: TKey}}
   */
  observe(key, weight = 1) {
    if (weight <= 0) return { event: 'increment' };

    const idx = this._indexByKey.get(key);
    if (idx !== undefined) {
      const entry = this._entries[idx];
      entry.count = addSaturating(entry.count, weight);

      if (idx === this._minIndex) {
        this._recomputeMinIndex();
      }
      return { event: 'increment' };
    }

    if (this._entries.length < this._capacity) {
      this._entries.push({ key, count: Math.min(weight, MAX_COUNT), error: 0 });
      this._indexByKey.set(key, this._entries.length - 1);
      this._recomputeMinIndex();
      return { event: 'insert' };
    }

    // Replace the minimum counter.
    const minIndex = this._minIndex;
    const minEntry = this._entries[minIndex];
    const replacedKey = minEntry.key;
    const prevMinCount = minEntry.count;

    this._indexByKey.delete(replacedKey);

    minEntry.key = key;
    minEntry.error = prevMinCount;
    minEntry.count = addSaturating(prevMinCount, weight);

    this._indexByKey.set(key, minIndex);
    this._recomputeMinIndex();

    return { event: 'replace', replacedKey };
  }

  clear() {
    this._entries.length = 0;
    this._indexByKey.clear();
    this._minIndex = -1;
  }

  /**
   * Returns a sorted copy (descending by count).
   * @param {{limit?: number}} [options]
   * @returns {Array<SpaceSavingEntry<TKey>>}
   */
  snapshot(options = {}) {
    const { limit } = options;
    const sorted = this._entries
      .map((e) => ({ ...e }))
      .sort((a, b) => b.count - a.count);
    if (limit === undefined) return sorted;
    return sorted.slice(0, limit);
  }

  _recomputeMinIndex() {
    if (this._entries.length === 0) {
      this._minIndex = -1;
      return;
    }
    let minIndex = 0;
    let minCount = this._entries[0].count;
    for (let i = 1; i < this._entries.length; i++) {
      const c = this._entries[i].count;
      if (c < minCount) {
        minCount = c;
        minIndex = i;
      }
    }
    this._minIndex = minIndex;
  }
}

