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

