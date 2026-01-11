import assert from 'node:assert/strict';
import test from 'node:test';

import { HotspotTracker } from '../src/perf/hotspot_tracker.js';

test('HotspotTracker reports a tight loop PC as the dominant hotspot', () => {
  const tracker = new HotspotTracker({ capacity: 8 });

  // Simulate a loop at 0x1000 with a 3-instruction basic block executed many times.
  for (let i = 0; i < 10_000; i++) tracker.recordBlock(0x1000, 3);

  // Some noise elsewhere.
  for (let i = 0; i < 100; i++) tracker.recordBlock(0x2000, 10);

  const [top] = tracker.snapshot({ limit: 1 });

  assert.equal(top.pc, '0x1000');
  assert.equal(top.hits, 10_000);
  assert(top.percent_of_total > 90);
});

test('HotspotTracker ignores NaN/non-positive instruction counts', () => {
  const tracker = new HotspotTracker({ capacity: 8 });

  tracker.recordBlock(0x1000, NaN);
  tracker.recordBlock(0x1000, 0);
  tracker.recordBlock(0x1000, -5);

  assert.equal(tracker.totalInstructions, 0);
  assert.deepEqual(tracker.snapshot({ limit: 1 }), []);
});

test('HotspotTracker saturates instruction counts on Infinity to keep percentages finite', () => {
  const tracker = new HotspotTracker({ capacity: 8 });

  tracker.recordBlock(0x1000, Infinity);
  const [top] = tracker.snapshot({ limit: 1 });

  assert.equal(tracker.totalInstructions, Number.MAX_SAFE_INTEGER);
  assert.equal(top.pc, '0x1000');
  assert.equal(top.hits, 1);
  assert.equal(top.percent_of_total, 100);
});
