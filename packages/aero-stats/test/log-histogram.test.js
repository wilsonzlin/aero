import assert from 'node:assert/strict';
import test from 'node:test';

import { LogHistogram } from '../src/log-histogram.js';

function nearestRankQuantile(sorted, q) {
  if (sorted.length === 0) return Number.NaN;
  if (q <= 0) return sorted[0];
  if (q >= 1) return sorted[sorted.length - 1];
  const rank = Math.ceil(q * sorted.length);
  return sorted[rank - 1];
}

test('LogHistogram: uniform distribution quantiles are accurate', () => {
  const hist = new LogHistogram({ subBucketCount: 1024, maxExponent: 20 });
  const samples = [];

  for (let i = 1; i <= 10_000; i += 1) {
    hist.record(i);
    samples.push(i);
  }
  samples.sort((a, b) => a - b);

  for (const q of [0.5, 0.95, 0.99]) {
    const expected = nearestRankQuantile(samples, q);
    const got = hist.quantile(q);
    assert.ok(
      Math.abs(got - expected) <= 16,
      `q=${q} expected≈${expected} got=${got}`,
    );
  }
});

test('LogHistogram: bimodal distribution quantiles land in the correct mode', () => {
  const hist = new LogHistogram({ subBucketCount: 1024, maxExponent: 20 });
  const samples = [];

  for (let i = 0; i < 9_000; i += 1) {
    hist.record(10);
    samples.push(10);
  }
  for (let i = 0; i < 1_000; i += 1) {
    hist.record(100);
    samples.push(100);
  }

  samples.sort((a, b) => a - b);

  const p50 = hist.quantile(0.5);
  const p95 = hist.quantile(0.95);
  const p99 = hist.quantile(0.99);

  assert.ok(Math.abs(p50 - 10) < 0.1, `p50 expected≈10 got=${p50}`);
  assert.ok(Math.abs(p95 - 100) < 0.2, `p95 expected≈100 got=${p95}`);
  assert.ok(Math.abs(p99 - 100) < 0.2, `p99 expected≈100 got=${p99}`);

  assert.equal(nearestRankQuantile(samples, 0.95), 100);
  assert.equal(nearestRankQuantile(samples, 0.99), 100);
});

test('LogHistogram: merge preserves quantiles', () => {
  const left = new LogHistogram({ subBucketCount: 1024, maxExponent: 20 });
  const right = new LogHistogram({ subBucketCount: 1024, maxExponent: 20 });
  const all = new LogHistogram({ subBucketCount: 1024, maxExponent: 20 });

  for (let i = 1; i <= 10_000; i += 1) {
    all.record(i);
    if (i <= 5_000) left.record(i);
    else right.record(i);
  }

  left.merge(right);

  for (const q of [0.5, 0.95, 0.99]) {
    assert.ok(Math.abs(left.quantile(q) - all.quantile(q)) < 1e-9);
  }
});

