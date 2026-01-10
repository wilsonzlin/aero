import assert from 'node:assert/strict';
import test from 'node:test';

import { DnsCache } from '../src/dns/cache.js';

test('DNS cache TTL expiry and max TTL clamp', () => {
  let nowMs = 0;
  const cache = new DnsCache(10, 2, () => nowMs);

  cache.set('k', Buffer.from([1]), 100);
  assert.deepEqual(cache.get('k'), Buffer.from([1]));

  nowMs = 1999;
  assert.deepEqual(cache.get('k'), Buffer.from([1]));

  nowMs = 2000;
  assert.equal(cache.get('k'), null);
});
