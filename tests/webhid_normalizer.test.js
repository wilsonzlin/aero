import assert from 'node:assert/strict';
import test from 'node:test';

import { MAX_RANGE_CONTIGUITY_CHECK_LEN, normalizeCollections } from '../web/src/hid/webhid_normalize.ts';

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
  assert.deepEqual(out.usages, [5, 5]);
});

test('webhid_normalize: downgrades non-contiguous ranges', () => {
  const out = normalizeSingleItem(makeItem({ isRange: true, usages: [1, 3, 4], usageMinimum: 1, usageMaximum: 4 }));
  assert.equal(out.isRange, false);
  assert.deepEqual(out.usages, [1, 3, 4]);
});

test('webhid_normalize: does not iterate huge usages lists for isRange items', () => {
  const hugeUsages = {
    length: MAX_RANGE_CONTIGUITY_CHECK_LEN + 1,
    [Symbol.iterator]() {
      throw new Error('should not iterate huge usages');
    },
  };

  const out = normalizeSingleItem(
    makeItem({
      isRange: true,
      usages: hugeUsages,
      usageMinimum: 1,
      usageMaximum: 12345,
    }),
  );

  assert.equal(out.isRange, true);
  assert.ok(Array.isArray(out.usages));
  assert.ok(out.usages.length <= 2);
  assert.deepEqual(out.usages, [1, 12345]);
});

test('webhid_normalize: accepts wrap (alias for isWrapped)', () => {
  const item = makeItem();
  delete item.isWrapped;
  item.wrap = true;

  const out = normalizeSingleItem(item);
  assert.equal(out.isWrapped, true);
  assert.equal('wrap' in out, false);
});

test('webhid_normalize: derives isRelative when omitted', () => {
  const item = makeItem({ isAbsolute: false });
  delete item.isRelative;

  const out = normalizeSingleItem(item);
  assert.equal(out.isAbsolute, false);
  assert.equal(out.isRelative, true);
});

test('webhid_normalize: rejects ambiguous single-usage ranges', () => {
  assert.throws(
    () => normalizeSingleItem(makeItem({ isRange: true, usages: [5], usageMinimum: 5, usageMaximum: 6 })),
    /isRange=true/,
  );
});
