import assert from 'node:assert/strict';
import test from 'node:test';

import { isPrivatePtrQname } from '../src/dns/resolver.js';

test('isPrivatePtrQname detects private IPv4 in-addr.arpa names (case-insensitive suffix)', () => {
  assert.equal(isPrivatePtrQname('1.0.0.127.in-addr.arpa'), true);
  assert.equal(isPrivatePtrQname('1.0.0.127.IN-ADDR.ARPA'), true);
  assert.equal(isPrivatePtrQname('8.8.8.8.in-addr.arpa'), false);
});

test('isPrivatePtrQname detects private IPv6 ip6.arpa names (case-insensitive hex + suffix)', () => {
  // ::1 (loopback) in nibble-reversed form.
  const loopback = '1.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.ip6.arpa';
  assert.equal(isPrivatePtrQname(loopback), true);
  assert.equal(isPrivatePtrQname(loopback.toUpperCase()), true);
});

