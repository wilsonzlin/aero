import assert from 'node:assert/strict';
import test from 'node:test';
import { TokenBucketRateLimiter } from '../src/dns/rateLimit.js';

test('TokenBucketRateLimiter prunes stale buckets when the map grows large', () => {
  let now = 0;
  const limiter = new TokenBucketRateLimiter(1, 1, () => now);

  for (let i = 0; i <= 10_000; i++) {
    assert.equal(limiter.allow(`client-${i}`), true);
  }

  assert.equal(limiter.bucketCount(), 10_001);

  // Advance time past the 10 minute prune threshold and create a new bucket, which should trigger
  // pruning of the old inactive entries.
  now = 11 * 60 * 1000;
  assert.equal(limiter.allow('fresh-client'), true);

  assert.equal(limiter.bucketCount(), 1);

  // Previously seen keys should have been evicted.
  assert.equal(limiter.allow('client-0'), true);
  assert.equal(limiter.bucketCount(), 2);
});

