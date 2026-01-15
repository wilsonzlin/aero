import assert from 'node:assert/strict';
import test from 'node:test';

import {
  evaluateTcpHostPolicy,
  hostnameMatchesPattern,
  normalizeHostname,
  parseHostnamePattern,
  parseHostnamePatterns,
} from '../src/security/egressPolicy.js';
import { isPublicIpAddress } from '../src/security/ipPolicy.js';

test('wildcard matching: exact match does not match suffixes/prefixes', () => {
  const pattern = parseHostnamePattern('example.com');
  assert.equal(hostnameMatchesPattern('example.com', pattern), true);
  assert.equal(hostnameMatchesPattern('badexample.com', pattern), false);
  assert.equal(hostnameMatchesPattern('good.example.com', pattern), false);
});

test('wildcard matching: *.example.com matches subdomains but not the apex', () => {
  const pattern = parseHostnamePattern('*.example.com');
  assert.equal(hostnameMatchesPattern('example.com', pattern), false);
  assert.equal(hostnameMatchesPattern('a.example.com', pattern), true);
  assert.equal(hostnameMatchesPattern('a.b.example.com', pattern), true);
  assert.equal(hostnameMatchesPattern('badexample.com', pattern), false);
});

test('hostname normalization: lowercases and strips trailing dot', () => {
  assert.equal(normalizeHostname('Example.COM.'), 'example.com');
});

test('hostname normalization: IDNA punycode normalization', () => {
  // "bücher.example" -> "xn--bcher-kva.example"
  assert.equal(normalizeHostname('BÜCHER.example'), 'xn--bcher-kva.example');
});

test('hostname normalization: rejects invalid hostnames', () => {
  assert.throws(() => normalizeHostname('exa_mple.com'));
  assert.throws(() => normalizeHostname('-example.com'));
  assert.throws(() => normalizeHostname('example..com'));
});

test('parseHostnamePatterns: trims, skips empties, normalizes', () => {
  const patterns = parseHostnamePatterns(' Example.COM, ,*.EXAMPLE.com ');
  assert.equal(patterns.length, 2);
  assert.deepEqual(patterns[0], { kind: 'exact', hostname: 'example.com' });
  assert.deepEqual(patterns[1], { kind: 'wildcard', suffix: 'example.com' });
});

test('IP policy: blocks private/reserved IPs and allows public IPs', () => {
  assert.equal(isPublicIpAddress('127.0.0.1'), false);
  assert.equal(isPublicIpAddress('10.0.0.1'), false);
  assert.equal(isPublicIpAddress('192.168.1.2'), false);
  assert.equal(isPublicIpAddress('8.8.8.8'), true);

  assert.equal(isPublicIpAddress('::1'), false);
  assert.equal(isPublicIpAddress('fd00::1'), false);
  assert.equal(isPublicIpAddress('fe80::1'), false);
  assert.equal(isPublicIpAddress('2001:4860:4860::8888'), true);
  assert.equal(isPublicIpAddress('::ffff:127.0.0.1'), false);

  // IPv4-compatible ::/96 should apply IPv4 policy (prevents private-range bypass).
  assert.equal(isPublicIpAddress('::10.0.0.1'), false);
  assert.equal(isPublicIpAddress('::192.168.1.2'), false);
  assert.equal(isPublicIpAddress('::8.8.8.8'), true);
});

test('IP policy: rejects malformed IPv6 literals', () => {
  // These are not valid IPv6 strings but can be accidentally accepted by
  // overly-permissive parsers.
  assert.equal(isPublicIpAddress('1:2:3:4:5:6:7:8:'), false);
  assert.equal(isPublicIpAddress(':1:2:3:4:5:6:7:8'), false);
  assert.equal(isPublicIpAddress('1:::2'), false);
  assert.equal(isPublicIpAddress(':::1'), false);
  assert.equal(isPublicIpAddress('1:2:3:4:5:6:7:8::'), false);
  assert.equal(isPublicIpAddress(`::${'1'.repeat(1000)}`), false);

  // IPv4-mapped IPv6 literals must use a canonical dotted-decimal tail.
  assert.equal(isPublicIpAddress('::ffff:001.002.003.004'), false);
  assert.equal(isPublicIpAddress('::ffff:010.0.0.1'), false);
});

test('IP policy: understands common non-canonical IPv4 forms', () => {
  // These forms are accepted by Node's resolver (`dns.lookup` / `getaddrinfo`).
  assert.equal(isPublicIpAddress('0177.0.0.1'), false); // octal => 127.0.0.1
  assert.equal(isPublicIpAddress('0x7f.0.0.1'), false); // hex => 127.0.0.1
  assert.equal(isPublicIpAddress('2130706433'), false); // 32-bit integer => 127.0.0.1
  assert.equal(isPublicIpAddress('127.1'), false); // shorthand => 127.0.0.1

  assert.equal(isPublicIpAddress('010.0.0.1'), true); // octal => 8.0.0.1
  assert.equal(isPublicIpAddress('08.0.0.1'), true); // decimal fallback => 8.0.0.1

  // Trailing-dot dotted-quad parsing follows getaddrinfo behaviour: decimal-only.
  assert.equal(isPublicIpAddress('010.0.0.1.'), false); // => 10.0.0.1 (RFC1918)
  assert.equal(isPublicIpAddress('0177.0.0.1.'), true); // => 177.0.0.1 (public)

  // When any dotted-quad component triggers decimal fallback (e.g. "08"), the
  // whole address should be parsed as decimal, not mixed octal/decimal.
  assert.equal(isPublicIpAddress('010.08.0.1'), false); // => 10.8.0.1 (RFC1918)
  assert.equal(isPublicIpAddress('010.09.0.1'), false); // => 10.9.0.1 (RFC1918)

  // Mixed hex + "08"-style components are not accepted by getaddrinfo.
  assert.equal(isPublicIpAddress('0x08.0008.0.1'), false);
  assert.equal(isPublicIpAddress('1.08.0x1.1'), false);
});

test('host policy: allow/deny lists apply to IP-literal targets', () => {
  const allow = parseHostnamePatterns('8.8.8.8');
  const block = parseHostnamePatterns('8.8.8.8');

  assert.deepEqual(evaluateTcpHostPolicy('8.8.8.8', { allowed: allow, blocked: [], requireDnsName: false }), {
    allowed: true,
    target: { kind: 'ip', ip: '8.8.8.8', version: 4 },
  });

  const denied = evaluateTcpHostPolicy('8.8.8.8', { allowed: [], blocked: block, requireDnsName: false });
  assert.equal(denied.allowed, false);
  if (!denied.allowed) assert.equal(denied.reason, 'blocked-by-host-policy');

  const deniedOverride = evaluateTcpHostPolicy('8.8.8.8', { allowed: allow, blocked: block, requireDnsName: false });
  assert.equal(deniedOverride.allowed, false);
  if (!deniedOverride.allowed) assert.equal(deniedOverride.reason, 'blocked-by-host-policy');

  const deniedDnsName = evaluateTcpHostPolicy('8.8.8.8', { allowed: allow, blocked: [], requireDnsName: true });
  assert.equal(deniedDnsName.allowed, false);
  if (!deniedDnsName.allowed) assert.equal(deniedDnsName.reason, 'ip-literal-disallowed');
});

test('host policy: TCP_REQUIRE_DNS_NAME rejects non-canonical IPv4 literals', () => {
  const policy = { allowed: [], blocked: [], requireDnsName: true };
  for (const host of [
    '0177.0.0.1', // octal dotted-quad
    '0x7f.0.0.1', // hex dotted-quad
    '2130706433', // 32-bit integer
    '127.1', // shorthand
    '010.0.0.1', // octal => 8.0.0.1
    '08.0.0.1', // decimal fallback dotted-quad
    '8.8.8.8.', // trailing dot dotted-quad
    '8.8.8.8..', // normalized (trailing dots stripped) but still numeric
    '010.0.0.1.', // trailing dot forces decimal => 10.0.0.1
  ]) {
    const decision = evaluateTcpHostPolicy(host, policy);
    assert.equal(decision.allowed, false);
    if (!decision.allowed) assert.equal(decision.reason, 'ip-literal-disallowed');
  }
});

test('host policy: allow/deny lists match IPv6 literals regardless of formatting', () => {
  const allow = parseHostnamePatterns('2001:DB8::ABCD');
  const block = parseHostnamePatterns('2001:db8:0:0:0:0:0:abcd');

  const allowed = evaluateTcpHostPolicy('2001:db8::abcd', { allowed: allow, blocked: [], requireDnsName: false });
  assert.equal(allowed.allowed, true);
  if (allowed.allowed) {
    assert.deepEqual(allowed.target, { kind: 'ip', ip: '2001:0db8:0000:0000:0000:0000:0000:abcd', version: 6 });
  }

  const denied = evaluateTcpHostPolicy('2001:db8::abcd', { allowed: [], blocked: block, requireDnsName: false });
  assert.equal(denied.allowed, false);
  if (!denied.allowed) assert.equal(denied.reason, 'blocked-by-host-policy');
});

