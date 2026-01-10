import assert from 'node:assert/strict';
import test from 'node:test';

import {
  evaluateTcpHostPolicy,
  hostnameMatchesPattern,
  normalizeHostname,
  parseHostnamePattern,
  parseHostnamePatterns,
} from './egressPolicy.js';
import { isPublicIpAddress } from './ipPolicy.js';

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
});

test("host policy: allow/deny lists apply to IP-literal targets", () => {
  const allow = parseHostnamePatterns("8.8.8.8");
  const block = parseHostnamePatterns("8.8.8.8");

  assert.deepEqual(evaluateTcpHostPolicy("8.8.8.8", { allowed: allow, blocked: [], requireDnsName: false }), {
    allowed: true,
    target: { kind: "ip", ip: "8.8.8.8", version: 4 },
  });

  const denied = evaluateTcpHostPolicy("8.8.8.8", { allowed: [], blocked: block, requireDnsName: false });
  assert.equal(denied.allowed, false);
  if (!denied.allowed) assert.equal(denied.reason, "blocked-by-host-policy");

  const deniedOverride = evaluateTcpHostPolicy("8.8.8.8", { allowed: allow, blocked: block, requireDnsName: false });
  assert.equal(deniedOverride.allowed, false);
  if (!deniedOverride.allowed) assert.equal(deniedOverride.reason, "blocked-by-host-policy");

  const deniedDnsName = evaluateTcpHostPolicy("8.8.8.8", { allowed: allow, blocked: [], requireDnsName: true });
  assert.equal(deniedDnsName.allowed, false);
  if (!deniedDnsName.allowed) assert.equal(deniedDnsName.reason, "ip-literal-disallowed");
});
