import assert from 'node:assert/strict';
import test from 'node:test';
import { isOriginAllowed } from '../src/middleware/originGuard.js';

test('isOriginAllowed matches exact origin', () => {
  assert.equal(isOriginAllowed('https://example.com', ['https://example.com']), true);
  assert.equal(isOriginAllowed('https://evil.com', ['https://example.com']), false);
});

test('isOriginAllowed handles wildcard', () => {
  assert.equal(isOriginAllowed('https://evil.com', ['*']), true);
});

