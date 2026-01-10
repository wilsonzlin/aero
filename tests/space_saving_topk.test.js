import assert from 'node:assert/strict';
import test from 'node:test';

import { SpaceSavingTopK } from '../src/perf/space_saving_topk.js';

test('SpaceSavingTopK keeps bounded memory', () => {
  const k = 3;
  const topk = new SpaceSavingTopK(k);
  for (let i = 0; i < 10_000; i++) {
    topk.observe(i, 1);
  }
  assert.equal(topk.size, k);
  assert.equal(topk.entries.length, k);
});

test('SpaceSavingTopK retains heavy hitters in a noisy stream', () => {
  const topk = new SpaceSavingTopK(10);

  for (let i = 0; i < 10_000; i++) topk.observe('hot', 1);
  for (let i = 0; i < 9_000; i++) topk.observe('warm', 1);

  for (let i = 0; i < 10_000; i++) topk.observe(`cold_${i}`, 1);

  const snapshot = topk.snapshot();
  const keys = snapshot.map((e) => e.key);

  assert(keys.includes('hot'));
  assert(keys.includes('warm'));
  assert.equal(snapshot[0].key, 'hot');
});
