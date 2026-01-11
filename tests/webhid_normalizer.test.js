import assert from 'node:assert/strict';
import test from 'node:test';

import { normalizeCollections } from '../web/src/hid/webhid_normalize.ts';

function makeItem(overrides = {}) {
  return {
    usagePage: 0x07,
    usages: [],
    usageMinimum: 0,
    usageMaximum: 0,
    reportSize: 1,
    reportCount: 1,
    unitExponent: 0,
    unit: 0,
    logicalMinimum: 0,
    logicalMaximum: 1,
    physicalMinimum: 0,
    physicalMaximum: 0,
    strings: [],
    stringMinimum: 0,
    stringMaximum: 0,
    designators: [],
    designatorMinimum: 0,
    designatorMaximum: 0,
    isAbsolute: true,
    isArray: false,
    isBufferedBytes: false,
    isConstant: false,
    isLinear: true,
    isRange: false,
    isRelative: false,
    isVolatile: false,
    hasNull: false,
    hasPreferredState: true,
    isWrapped: false,
    ...overrides,
  };
}

function normalizeSingleItem(item) {
  const collections = [
    {
      usagePage: 1,
      usage: 6,
      type: 'application',
      children: [],
      inputReports: [{ reportId: 0, items: [item] }],
      outputReports: [],
      featureReports: [],
    },
  ];
  return normalizeCollections(collections)[0].inputReports[0].items[0];
}

test('webhid_normalize: accepts isRange with single usage', () => {
  const out = normalizeSingleItem(makeItem({ isRange: true, usages: [5], usageMinimum: 5, usageMaximum: 5 }));
  assert.equal(out.isRange, true);
  assert.deepEqual(out.usages, [5]);
});

test('webhid_normalize: downgrades non-contiguous ranges', () => {
  const out = normalizeSingleItem(makeItem({ isRange: true, usages: [1, 3, 4], usageMinimum: 1, usageMaximum: 4 }));
  assert.equal(out.isRange, false);
  assert.deepEqual(out.usages, [1, 3, 4]);
});
