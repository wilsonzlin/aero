import assert from 'node:assert/strict';
import test from 'node:test';

import { RunningStats } from '../src/running-stats.js';

test('RunningStats: mean/variance match batch computation', () => {
  const stats = new RunningStats();
  for (const v of [1, 2, 3, 4]) stats.push(v);

  assert.equal(stats.count, 4);
  assert.equal(stats.mean, 2.5);
  assert.equal(stats.variancePopulation, 1.25);
  assert.equal(stats.stdevPopulation, Math.sqrt(1.25));
  assert.equal(stats.min, 1);
  assert.equal(stats.max, 4);
  assert.equal(stats.sum, 10);
});

test('RunningStats: merge equals single-pass (within float epsilon)', () => {
  const left = new RunningStats();
  const right = new RunningStats();
  const all = new RunningStats();

  for (let i = 1; i <= 10_000; i += 1) {
    const v = i / 10;
    all.push(v);
    if (i <= 5_000) left.push(v);
    else right.push(v);
  }

  left.merge(right);

  assert.equal(left.count, all.count);
  assert.ok(Math.abs(left.mean - all.mean) < 1e-9);
  assert.ok(Math.abs(left.variancePopulation - all.variancePopulation) < 1e-8);
  assert.equal(left.min, all.min);
  assert.equal(left.max, all.max);
  assert.ok(Math.abs(left.sum - all.sum) < 1e-9);
});
