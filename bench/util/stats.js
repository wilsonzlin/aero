/**
 * @param {number[]} values
 * @returns {number}
 */
function mean(values) {
  if (values.length === 0) return NaN;
  let sum = 0;
  for (const v of values) sum += v;
  return sum / values.length;
}

/**
 * Sample standard deviation (n-1).
 * @param {number[]} values
 * @returns {number}
 */
function stdev(values) {
  if (values.length <= 1) return 0;
  const m = mean(values);
  let sumSq = 0;
  for (const v of values) sumSq += (v - m) ** 2;
  return Math.sqrt(sumSq / (values.length - 1));
}

/**
 * @param {number[]} values
 * @returns {number}
 */
function median(values) {
  if (values.length === 0) return NaN;
  const sorted = [...values].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  if (sorted.length % 2 === 1) return sorted[mid];
  return (sorted[mid - 1] + sorted[mid]) / 2;
}

/**
 * @param {number[]} values
 */
function min(values) {
  return values.length ? Math.min(...values) : NaN;
}

/**
 * @param {number[]} values
 */
function max(values) {
  return values.length ? Math.max(...values) : NaN;
}

/**
 * @param {number[]} values
 */
export function computeStats(values) {
  const m = mean(values);
  const s = stdev(values);
  const cov = Number.isFinite(m) && m !== 0 ? s / m : 0;
  return {
    samples: values.length,
    median: median(values),
    mean: m,
    stdev: s,
    cov,
    min: min(values),
    max: max(values)
  };
}
