import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import fc from 'fast-check';

import { hostnameMatchesPattern, normalizeHostname, parseHostnamePattern } from '../../src/security/egressPolicy.js';

const FC_NUM_RUNS = process.env.FC_NUM_RUNS ? Number(process.env.FC_NUM_RUNS) : process.env.CI ? 200 : 500;
const FC_TIME_LIMIT_MS = process.env.CI ? 2_000 : 5_000;

const hostLabelChar = fc.constantFrom(...'abcdefghijklmnopqrstuvwxyz0123456789'.split(''), '-');

const hostLabelArb = fc
  .array(hostLabelChar, { minLength: 1, maxLength: 20 })
  .map((chars) => chars.join(''))
  .filter((s) => /^[a-z]/.test(s) && !s.startsWith('xn--') && !s.startsWith('-') && !s.endsWith('-'));

const domainArb = fc.array(hostLabelArb, { minLength: 2, maxLength: 4 }).map((labels) => labels.join('.'));

describe('hostname normalization & wildcard matching (property)', () => {
  it(
    'normalizeHostname is idempotent for generated hostnames',
    { timeout: 10_000 },
    () => {
      fc.assert(
        fc.property(domainArb, fc.boolean(), fc.boolean(), (domain, upper, trailingDot) => {
          const decorated = `${upper ? domain.toUpperCase() : domain}${trailingDot ? '.' : ''}`;
          const norm1 = normalizeHostname(decorated);
          assert.ok(norm1);
          assert.equal(normalizeHostname(norm1), norm1);
          assert.equal(norm1, norm1.toLowerCase());
          assert.equal(norm1.endsWith('.'), false);
        }),
        { numRuns: FC_NUM_RUNS, interruptAfterTimeLimit: FC_TIME_LIMIT_MS },
      );
    },
  );

  it(
    'wildcard patterns only match proper subdomains',
    { timeout: 10_000 },
    () => {
      fc.assert(
        fc.property(domainArb, hostLabelArb, (base, sub) => {
          const normalizedBase = normalizeHostname(base);
          const pattern = parseHostnamePattern(`*.${normalizedBase}`);
          const subdomain = normalizeHostname(`${sub}.${normalizedBase}`);

          assert.equal(hostnameMatchesPattern(subdomain, pattern), true);
          assert.equal(hostnameMatchesPattern(normalizedBase, pattern), false);
          assert.equal(hostnameMatchesPattern(normalizeHostname(`${subdomain}.evil.com`), pattern), false);
        }),
        { numRuns: FC_NUM_RUNS, interruptAfterTimeLimit: FC_TIME_LIMIT_MS },
      );
    },
  );
});
