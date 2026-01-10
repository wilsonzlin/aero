import assert from 'node:assert/strict';
import { PassThrough } from 'node:stream';
import { describe, it } from 'node:test';

import fc from 'fast-check';

import { TcpTargetParseError, parseTcpTarget } from '../../src/protocol/tcpTarget.js';
import { TcpTargetPolicyError, enforceTcpTargetPolicy } from '../../src/protocol/tcpTargetPolicy.js';
import { handleTcpProxyUpgrade } from '../../src/routes/tcpProxy.js';

const FC_NUM_RUNS = process.env.FC_NUM_RUNS ? Number(process.env.FC_NUM_RUNS) : process.env.CI ? 200 : 500;
const FC_TIME_LIMIT_MS = process.env.CI ? 2_000 : 5_000;

function ipv4FromBytes(bytes: readonly number[]): string {
  return `${bytes[0]}.${bytes[1]}.${bytes[2]}.${bytes[3]}`;
}

const privateIpv4Arb = fc.oneof(
  fc.tuple(fc.constant(10), fc.nat(255), fc.nat(255), fc.nat(255)).map((t) => ipv4FromBytes(t)),
  fc.tuple(fc.constant(127), fc.nat(255), fc.nat(255), fc.nat(255)).map((t) => ipv4FromBytes(t)),
  fc.tuple(fc.constant(192), fc.constant(168), fc.nat(255), fc.nat(255)).map((t) => ipv4FromBytes(t)),
  fc
    .tuple(fc.constant(172), fc.integer({ min: 16, max: 31 }), fc.nat(255), fc.nat(255))
    .map((t) => ipv4FromBytes(t)),
  fc.tuple(fc.constant(169), fc.constant(254), fc.nat(255), fc.nat(255)).map((t) => ipv4FromBytes(t)),
);

const ipv6Arb = fc
  .array(fc.integer({ min: 0, max: 0xffff }), { minLength: 8, maxLength: 8 })
  .map((hextets) => hextets.map((h) => h.toString(16)).join(':'));

const privateIpv6Arb = fc.oneof(
  fc.constant('::1'),
  fc
    .array(fc.integer({ min: 0, max: 0xffff }), { minLength: 7, maxLength: 7 })
    .chain((tail) =>
      fc.integer({ min: 0xfc00, max: 0xfdff }).map((head) => [head, ...tail]),
    )
    .map((hextets) => hextets.map((h) => h.toString(16)).join(':')),
  fc
    .array(fc.integer({ min: 0, max: 0xffff }), { minLength: 7, maxLength: 7 })
    .chain((tail) =>
      fc.integer({ min: 0xfe80, max: 0xfebf }).map((head) => [head, ...tail]),
    )
    .map((hextets) => hextets.map((h) => h.toString(16)).join(':')),
);

function parseQuery(params: Record<string, string | undefined>): unknown {
  const sp = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) {
    if (typeof v === 'string') sp.set(k, v);
  }
  return parseTcpTarget(sp);
}

describe('tcp target parsing + policy (property)', () => {
  it(
    'parseTcpTarget only throws TcpTargetParseError on arbitrary query inputs',
    { timeout: 10_000 },
    () => {
      fc.assert(
        fc.property(
          fc.oneof(fc.constant(undefined), fc.string()),
          fc.oneof(fc.constant(undefined), fc.string()),
          fc.oneof(fc.constant(undefined), fc.string()),
          fc.oneof(fc.constant(undefined), fc.string()),
          (target, host, port, v) => {
            try {
              const parsed = parseQuery({ target, host, port, v });
              assert.equal(typeof parsed, 'object');
              assert.ok(parsed);
              const t = parsed as any;
              assert.equal(typeof t.host, 'string');
              assert.equal(typeof t.port, 'number');
              assert.equal(t.version, 1);
            } catch (err) {
              assert.ok(err instanceof TcpTargetParseError);
              assert.ok(typeof err.code === 'string');
            }
          },
        ),
        { numRuns: FC_NUM_RUNS, interruptAfterTimeLimit: FC_TIME_LIMIT_MS },
      );
    },
  );

  it(
    'IPv6 targets must be bracketed in target= form',
    { timeout: 10_000 },
    () => {
      fc.assert(
        fc.property(ipv6Arb, fc.integer({ min: 1, max: 65535 }), (ip6, port) => {
          assert.throws(
            () => parseQuery({ target: `${ip6}:${port}` }),
            (err: unknown) => err instanceof TcpTargetParseError && err.code === 'ERR_TCP_INVALID_TARGET',
          );

          assert.deepEqual(parseQuery({ target: `[${ip6}]:${port}` }), {
            host: ip6,
            port,
            version: 1,
          });
        }),
        { numRuns: FC_NUM_RUNS, interruptAfterTimeLimit: FC_TIME_LIMIT_MS },
      );
    },
  );

  it(
    'private-IP blocking rejects private IPv4 targets',
    { timeout: 10_000 },
    () => {
      fc.assert(
        fc.property(privateIpv4Arb, (ip) => {
          assert.throws(
            () =>
              enforceTcpTargetPolicy(
                { host: ip, port: 443, version: 1 },
                { blockPrivateIp: true, portAllowlist: new Set([443]) },
              ),
            (err: unknown) => err instanceof TcpTargetPolicyError && err.code === 'ERR_TCP_POLICY_PRIVATE_IP',
          );
        }),
        { numRuns: FC_NUM_RUNS, interruptAfterTimeLimit: FC_TIME_LIMIT_MS },
      );
    },
  );

  it(
    'private-IP blocking rejects private IPv6 targets',
    { timeout: 10_000 },
    () => {
      fc.assert(
        fc.property(privateIpv6Arb, (ip) => {
          assert.throws(
            () =>
              enforceTcpTargetPolicy(
                { host: ip, port: 443, version: 1 },
                { blockPrivateIp: true, portAllowlist: new Set([443]) },
              ),
            (err: unknown) => err instanceof TcpTargetPolicyError && err.code === 'ERR_TCP_POLICY_PRIVATE_IP',
          );
        }),
        { numRuns: FC_NUM_RUNS, interruptAfterTimeLimit: FC_TIME_LIMIT_MS },
      );
    },
  );

  it(
    'private-IP blocking rejects hostnames resolving to private IPv4 addresses',
    { timeout: 10_000 },
    () => {
      fc.assert(
        fc.property(privateIpv4Arb, (ip) => {
          assert.throws(
            () =>
              enforceTcpTargetPolicy(
                { host: 'example.com', port: 443, version: 1 },
                {
                  blockPrivateIp: true,
                  portAllowlist: new Set([443]),
                  resolveHostnameToIps: () => [ip],
                },
              ),
            (err: unknown) => err instanceof TcpTargetPolicyError && err.code === 'ERR_TCP_POLICY_PRIVATE_IP',
          );
        }),
        { numRuns: FC_NUM_RUNS, interruptAfterTimeLimit: FC_TIME_LIMIT_MS },
      );
    },
  );

  it(
    'port allowlist is enforced for arbitrary ports',
    { timeout: 10_000 },
    () => {
      fc.assert(
        fc.property(fc.integer({ min: 1, max: 65535 }), (port) => {
          if (port === 80 || port === 443) {
            assert.deepEqual(
              enforceTcpTargetPolicy(
                { host: '8.8.8.8', port, version: 1 },
                { blockPrivateIp: true, portAllowlist: new Set([80, 443]) },
              ),
              { host: '8.8.8.8', port, version: 1 },
            );
            return;
          }

          assert.throws(
            () =>
              enforceTcpTargetPolicy(
                { host: '8.8.8.8', port, version: 1 },
                { blockPrivateIp: true, portAllowlist: new Set([80, 443]) },
              ),
            (err: unknown) => err instanceof TcpTargetPolicyError && err.code === 'ERR_TCP_POLICY_DISALLOWED_PORT',
          );
        }),
        { numRuns: FC_NUM_RUNS, interruptAfterTimeLimit: FC_TIME_LIMIT_MS },
      );
    },
  );

  it(
    'route handler never throws on arbitrary target= values (and returns an HTTP error)',
    { timeout: 10_000 },
    () => {
      fc.assert(
        fc.property(fc.string(), (target) => {
          const req = { url: `/tcp?target=${encodeURIComponent(target)}`, headers: {} } as any;
          const socket = new PassThrough();
          assert.doesNotThrow(() => handleTcpProxyUpgrade(req, socket, Buffer.alloc(0)));
        }),
        { numRuns: FC_NUM_RUNS, interruptAfterTimeLimit: FC_TIME_LIMIT_MS },
      );
    },
  );
});
