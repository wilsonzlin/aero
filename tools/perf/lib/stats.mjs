import { RunningStats } from "../../../packages/aero-stats/src/running-stats.js";

export function mean(values) {
  if (values.length === 0) return NaN;
  const stats = new RunningStats();
  for (const v of values) stats.push(v);
  return stats.mean;
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
  const stats = new RunningStats();
  for (const v of values) stats.push(v);
  return stats.stdevPopulation;
}

export function summarize(values) {
  const stats = new RunningStats();
  for (const v of values) stats.push(v);

  const m = stats.mean;
  const med = median(values);
  const sd = stats.stdevPopulation;
  return {
    n: stats.count,
    min: stats.min,
    max: stats.max,
    mean: m,
    median: med,
    stdev: sd,
    p05: quantile(values, 0.05),
    p95: quantile(values, 0.95),
    cv: Number.isFinite(m) && m !== 0 ? sd / m : NaN,
  };
}
