export function mean(values) {
  if (values.length === 0) return NaN;
  let total = 0;
  for (const v of values) total += v;
  return total / values.length;
}

export function median(values) {
  if (values.length === 0) return NaN;
  const sorted = [...values].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  if (sorted.length % 2 === 0) {
    return (sorted[mid - 1] + sorted[mid]) / 2;
  }
  return sorted[mid];
}

export function quantile(values, q) {
  if (values.length === 0) return NaN;
  if (q <= 0) return Math.min(...values);
  if (q >= 1) return Math.max(...values);

  const sorted = [...values].sort((a, b) => a - b);
  const idx = (sorted.length - 1) * q;
  const lo = Math.floor(idx);
  const hi = Math.ceil(idx);
  if (lo === hi) return sorted[lo];
  const t = idx - lo;
  return sorted[lo] * (1 - t) + sorted[hi] * t;
}

export function stddev(values) {
  if (values.length === 0) return NaN;
  const m = mean(values);
  let sumSq = 0;
  for (const v of values) {
    const d = v - m;
    sumSq += d * d;
  }
  return Math.sqrt(sumSq / values.length);
}

export function summarize(values) {
  const m = mean(values);
  const med = median(values);
  const sd = stddev(values);
  return {
    n: values.length,
    min: Math.min(...values),
    max: Math.max(...values),
    mean: m,
    median: med,
    stdev: sd,
    p05: quantile(values, 0.05),
    p95: quantile(values, 0.95),
    cv: Number.isFinite(m) && m !== 0 ? sd / m : NaN,
  };
}

