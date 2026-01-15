import assert from 'node:assert/strict';
import type http from 'node:http';
import { PassThrough } from 'node:stream';
import { describe, it } from 'node:test';

import fc from 'fast-check';

import { TcpTargetParseError, parseTcpTarget } from '../../src/protocol/tcpTarget.js';
import { handleTcpProxyUpgrade } from '../../src/routes/tcpProxy.js';
import { resolveTcpProxyTarget, TcpProxyTargetError } from '../../src/routes/tcpResolve.js';
import { handleTcpMuxUpgrade } from '../../src/routes/tcpMux.js';
import { validateTcpTargetPolicy } from '../../src/routes/tcpPolicy.js';
import { isPublicIpAddress } from '../../src/security/ipPolicy.js';

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

const RESOLVE_ENV = {
  TCP_ALLOWED_HOSTS: '',
  TCP_BLOCKED_HOSTS: '',
  TCP_REQUIRE_DNS_NAME: '0',
};

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
              const t = parsed as Record<string, unknown>;
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
    'IP egress policy rejects private IPv4 literals',
    { timeout: 10_000 },
    () => {
      fc.assert(
        fc.property(privateIpv4Arb, (ip) => {
          assert.equal(isPublicIpAddress(ip), false);
        }),
        { numRuns: FC_NUM_RUNS, interruptAfterTimeLimit: FC_TIME_LIMIT_MS },
      );
    },
  );

  it(
    'IP egress policy rejects private IPv6 literals',
    { timeout: 10_000 },
    () => {
      fc.assert(
        fc.property(privateIpv6Arb, (ip) => {
          assert.equal(isPublicIpAddress(ip), false);
        }),
        { numRuns: FC_NUM_RUNS, interruptAfterTimeLimit: FC_TIME_LIMIT_MS },
      );
    },
  );

  it(
    'resolveTcpProxyTarget rejects private IPv4 literals',
    { timeout: 10_000 },
    async () => {
      await fc.assert(
        fc.asyncProperty(privateIpv4Arb, async (ip) => {
          await assert.rejects(
            resolveTcpProxyTarget(ip, 443, { env: RESOLVE_ENV }),
            (err: unknown) =>
              err instanceof TcpProxyTargetError && err.kind === 'ip-policy' && err.statusCode === 403,
          );
        }),
        { numRuns: process.env.CI ? 100 : FC_NUM_RUNS, interruptAfterTimeLimit: FC_TIME_LIMIT_MS },
      );
    },
  );

  it(
    'resolveTcpProxyTarget rejects private IPv6 literals',
    { timeout: 10_000 },
    async () => {
      await fc.assert(
        fc.asyncProperty(privateIpv6Arb, async (ip) => {
          await assert.rejects(
            resolveTcpProxyTarget(ip, 443, { env: RESOLVE_ENV }),
            (err: unknown) =>
              err instanceof TcpProxyTargetError && err.kind === 'ip-policy' && err.statusCode === 403,
          );
        }),
        { numRuns: process.env.CI ? 100 : FC_NUM_RUNS, interruptAfterTimeLimit: FC_TIME_LIMIT_MS },
      );
    },
  );

  it('resolveTcpProxyTarget rejects hostnames that only resolve to private IPs', async () => {
    await assert.rejects(
      resolveTcpProxyTarget('localhost', 443, { env: RESOLVE_ENV }),
      (err: unknown) =>
        err instanceof TcpProxyTargetError && err.kind === 'ip-policy' && err.statusCode === 403,
    );
  });

  it(
    'TCP port allowlist is enforced for arbitrary ports',
    { timeout: 10_000 },
    () => {
      fc.assert(
        fc.property(fc.integer({ min: -1000, max: 70000 }), (port) => {
          const decision = validateTcpTargetPolicy('example.com', port, { allowedTargetPorts: [80, 443] });
          if (!Number.isInteger(port) || port < 1 || port > 65535) {
            assert.equal(decision.ok, false);
            if (!decision.ok) assert.equal(decision.status, 400);
            return;
          }

          if (port === 80 || port === 443) {
            assert.deepEqual(decision, { ok: true });
            return;
          }

          assert.equal(decision.ok, false);
          if (!decision.ok) assert.equal(decision.status, 403);
        }),
        { numRuns: FC_NUM_RUNS, interruptAfterTimeLimit: FC_TIME_LIMIT_MS },
      );
    },
  );

  it(
    'route handlers never throw on arbitrary inputs (they respond with safe HTTP/WS errors)',
    { timeout: 10_000 },
    () => {
      fc.assert(
        fc.property(fc.string(), fc.string(), (target, garbagePath) => {
          const socketTcp = new PassThrough();
          const reqTcp = { url: `/tcp?target=${encodeURIComponent(target)}`, headers: {} } as unknown as http.IncomingMessage;
          assert.doesNotThrow(() => handleTcpProxyUpgrade(reqTcp, socketTcp, Buffer.alloc(0)));

          const socketMux = new PassThrough();
          const reqMux = { url: `/${encodeURIComponent(garbagePath)}`, headers: {} } as unknown as http.IncomingMessage;
          assert.doesNotThrow(() => handleTcpMuxUpgrade(reqMux, socketMux, Buffer.alloc(0)));
        }),
        { numRuns: FC_NUM_RUNS, interruptAfterTimeLimit: FC_TIME_LIMIT_MS },
      );
    },
  );
});
