import assert from 'node:assert/strict';
import test from 'node:test';

import { normalizeWebHidReportItem } from '../src/hid/webhid_normalizer.ts';

test('normalizeWebHidReportItem: accepts isRange with single usage', () => {
  const out = normalizeWebHidReportItem({ isRange: true, usages: [5] });
  assert.equal(out.isRange, true);
  assert.deepEqual(out.usages, [5]);
});

test('normalizeWebHidReportItem: downgrades non-contiguous ranges', () => {
  const out = normalizeWebHidReportItem({ isRange: true, usages: [1, 3, 4] });
  assert.equal(out.isRange, false);
  assert.deepEqual(out.usages, [1, 3, 4]);
});

