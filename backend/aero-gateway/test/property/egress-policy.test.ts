import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import fc from 'fast-check';

import {
  evaluateTcpHostPolicy,
  normalizeHostname,
  parseHostnamePattern,
} from '../../src/security/egressPolicy.js';

const FC_NUM_RUNS = process.env.FC_NUM_RUNS ? Number(process.env.FC_NUM_RUNS) : process.env.CI ? 200 : 500;
const FC_TIME_LIMIT_MS = process.env.CI ? 2_000 : 5_000;

const ipv4Arb = fc
  .tuple(fc.nat(255), fc.nat(255), fc.nat(255), fc.nat(255))
  .map(([a, b, c, d]) => `${a}.${b}.${c}.${d}`);

const ipv6Arb = fc
  .array(fc.integer({ min: 0, max: 0xffff }), { minLength: 8, maxLength: 8 })
  .map((hextets) => hextets.map((h) => h.toString(16)).join(':'));

const hostLabelChar = fc.constantFrom(...'abcdefghijklmnopqrstuvwxyz0123456789'.split(''), '-');
const hostLabelArb = fc
  .array(hostLabelChar, { minLength: 1, maxLength: 20 })
  .map((chars) => chars.join(''))
  .filter((s) => /^[a-z]/.test(s) && !s.startsWith('xn--') && !s.startsWith('-') && !s.endsWith('-'));

const domainArb = fc.array(hostLabelArb, { minLength: 2, maxLength: 4 }).map((labels) => labels.join('.'));

describe('TCP hostname egress policy (property)', () => {
  it(
    'evaluateTcpHostPolicy never throws for arbitrary host strings',
    { timeout: 10_000 },
    () => {
      fc.assert(
        fc.property(fc.string({ maxLength: 512 }), (host) => {
          const decision = evaluateTcpHostPolicy(host, { allowed: [], blocked: [], requireDnsName: false });
          assert.equal(typeof decision.allowed, 'boolean');
          if (!decision.allowed) {
            assert.ok(typeof decision.reason === 'string');
            assert.ok(decision.message.length > 0);
          } else {
            assert.ok(decision.target.kind === 'hostname' || decision.target.kind === 'ip');
          }
        }),
        { numRuns: FC_NUM_RUNS, interruptAfterTimeLimit: FC_TIME_LIMIT_MS },
      );
    },
  );

  it(
    'TCP_REQUIRE_DNS_NAME=1 rejects IPv4/IPv6 literals (including bracketed IPv6)',
    { timeout: 10_000 },
    () => {
      const ipLiteralArb = fc.oneof(ipv4Arb, ipv6Arb, ipv6Arb.map((ip) => `[${ip}]`));
      fc.assert(
        fc.property(ipLiteralArb, (ip) => {
          const decision = evaluateTcpHostPolicy(ip, { allowed: [], blocked: [], requireDnsName: true });
          assert.equal(decision.allowed, false);
          if (!decision.allowed) assert.equal(decision.reason, 'ip-literal-disallowed');
        }),
        { numRuns: FC_NUM_RUNS, interruptAfterTimeLimit: FC_TIME_LIMIT_MS },
      );
    },
  );

  it(
    'hostname allowlist wildcard requires at least one subdomain',
    { timeout: 10_000 },
    () => {
      fc.assert(
        fc.property(domainArb, hostLabelArb, (base, sub) => {
          const normalizedBase = normalizeHostname(base);
          const allowed = [parseHostnamePattern(`*.${normalizedBase}`)];
          const policy = { allowed, blocked: [], requireDnsName: false };

          const subDecision = evaluateTcpHostPolicy(`${sub}.${normalizedBase}`, policy);
          assert.equal(subDecision.allowed, true);

          const baseDecision = evaluateTcpHostPolicy(normalizedBase, policy);
          assert.equal(baseDecision.allowed, false);
          if (!baseDecision.allowed) assert.equal(baseDecision.reason, 'not-allowed-by-host-policy');
        }),
        { numRuns: FC_NUM_RUNS, interruptAfterTimeLimit: FC_TIME_LIMIT_MS },
      );
    },
  );
});

