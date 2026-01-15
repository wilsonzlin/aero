import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import fastify from 'fastify';
import fc from 'fast-check';

import { loadConfig } from '../../src/config.js';
import { decodeDnsHeader } from '../../src/dns/codec.js';
import { setupMetrics } from '../../src/metrics.js';
import { decodeBase64UrlToBuffer } from '../../src/base64url.js';
import { setupDohRoutes } from '../../src/routes/doh.js';
import { SESSION_COOKIE_NAME, createSessionManager } from '../../src/session.js';

const FC_NUM_RUNS = process.env.FC_NUM_RUNS ? Number(process.env.FC_NUM_RUNS) : process.env.CI ? 200 : 500;
const FC_TIME_LIMIT_MS = process.env.CI ? 2_000 : 5_000;

describe('DoH GET dns= decoding and size limits (property)', () => {
  it(
    'base64url decoder never crashes on arbitrary strings',
    { timeout: 10_000 },
    () => {
      fc.assert(
        fc.property(fc.string(), (dns) => {
          try {
            const decoded = decodeBase64UrlToBuffer(dns);
            assert.ok(Buffer.isBuffer(decoded));
          } catch (err) {
            assert.ok(err instanceof Error);
          }
        }),
        { numRuns: FC_NUM_RUNS, interruptAfterTimeLimit: FC_TIME_LIMIT_MS },
      );
    },
  );

  it(
    'oversized dns= parameters are rejected before DNS resolution',
    { timeout: 10_000 },
    async () => {
      const app = fastify();
      const config = loadConfig({
        DNS_MAX_QUERY_BYTES: '512',
        DNS_QPS_PER_IP: '0',
        DNS_BURST_PER_IP: '0',
      });
      const metrics = setupMetrics(app);
      const sessions = createSessionManager(config, { warn: (_obj: unknown, _msg?: string) => {} });
      const { token } = sessions.issueSession(null);
      const cookie = `${SESSION_COOKIE_NAME}=${token}`;
      setupDohRoutes(app, config, metrics.dns, sessions);

      await app.ready();
      try {
        const numRuns = process.env.CI ? 50 : 100;
        await fc.assert(
          fc.asyncProperty(
             fc.array(fc.integer({ min: 0, max: 255 }), { minLength: 513, maxLength: 600 }),
             async (arr) => {
               const query = Buffer.from(arr);
               const dnsParam = query.toString('base64url');
               const res = await app.inject({ method: 'GET', url: `/dns-query?dns=${dnsParam}`, headers: { cookie } });
               assert.equal(res.statusCode, 413);

               // The handler should preserve the query ID in the error response (best-effort header
               // parsing without decoding the full payload).
               const header = decodeDnsHeader(res.rawPayload);
               assert.equal(header.id, query.readUInt16BE(0));
               const queryFlags = query.readUInt16BE(2);
               const expectedFlags =
                 0x8000 | // QR
                 (queryFlags & 0x7800) | // opcode
                 (queryFlags & 0x0100) | // RD
                 0x0080 | // RA
                 (queryFlags & 0x0010) | // CD
                 0x0001; // FORMERR (rcode=1)
               assert.equal(header.flags, expectedFlags);
             },
           ),
           { numRuns, interruptAfterTimeLimit: FC_TIME_LIMIT_MS },
         );
      } finally {
        await app.close();
      }
    },
  );
});
