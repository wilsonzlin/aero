import assert from 'node:assert/strict';
import test from 'node:test';
import { getVersionInfo } from '../src/version.js';

test('getVersionInfo returns provenance fields', () => {
  const info = getVersionInfo();
  assert.equal(typeof info.version, 'string');
  assert.equal(typeof info.gitSha, 'string');
  assert.equal(typeof info.builtAt, 'string');
});

