import assert from 'node:assert/strict';
import test from 'node:test';

import { buildPlan, parseContentRange } from '../index.js';

test('parseContentRange parses "bytes 0-0/10"', () => {
  assert.deepStrictEqual(parseContentRange('bytes 0-0/10'), {
    unit: 'bytes',
    start: 0,
    end: 0,
    total: 10,
    isUnsatisfied: false,
  });
});

test('parseContentRange parses "bytes */10"', () => {
  assert.deepStrictEqual(parseContentRange('bytes */10'), {
    unit: 'bytes',
    start: null,
    end: null,
    total: 10,
    isUnsatisfied: true,
  });
});

test('parseContentRange returns null for invalid strings', () => {
  assert.equal(parseContentRange('nope'), null);
  assert.equal(parseContentRange('bytes 0-0'), null);
  assert.equal(parseContentRange('bytes 0-0/'), null);
});

test('parseContentRange returns null for oversized/unsafe values', () => {
  assert.equal(parseContentRange(`bytes 0-0/${'1'.repeat(300)}`), null);
  assert.equal(parseContentRange(`bytes 0-0/${'1'.repeat(32)}`), null);
});

test('buildPlan produces a sequential aligned plan', () => {
  const plan = buildPlan({
    size: 10,
    chunkSize: 4,
    count: 5,
    mode: 'sequential',
    seed: null,
    unique: false,
  });
  assert.deepStrictEqual(plan, [
    { index: 0, start: 0, end: 3 },
    { index: 1, start: 4, end: 7 },
    { index: 2, start: 8, end: 9 },
    { index: 3, start: 0, end: 3 },
    { index: 4, start: 4, end: 7 },
  ]);
});

