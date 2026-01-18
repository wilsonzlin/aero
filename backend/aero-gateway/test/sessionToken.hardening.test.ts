import assert from 'node:assert/strict';
import test from 'node:test';

import { mintSessionToken } from '../src/session.js';

test('mintSessionToken: throws stable error when JSON.stringify throws', () => {
  const secret = Buffer.from('test-secret', 'utf8');
  const original = JSON.stringify;
  try {
    JSON.stringify = () => {
      throw new Error('boom');
    };

    assert.throws(
      () => mintSessionToken({ v: 1, sid: 'sid', exp: 1_700_000_000 }, secret),
      /Session token encoding failed/,
    );
  } finally {
    JSON.stringify = original;
  }
});

