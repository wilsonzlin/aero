import assert from 'node:assert/strict';
import test from 'node:test';
import { buildServer } from '../src/server.js';
import { decodeDnsHeader } from '../src/dns/codec.js';
import { makeTestConfig } from './testConfig.js';

const baseConfig = makeTestConfig({
  RATE_LIMIT_REQUESTS_PER_MINUTE: 1,
  TCP_ALLOW_PRIVATE_IPS: false,
  TCP_PROXY_MAX_CONNECTIONS: 64,
  DNS_ALLOW_ANY: false,
  DNS_ALLOW_PRIVATE_PTR: false,
});

test('rate limiter rejects when the per-minute budget is exceeded', async () => {
  const { app } = buildServer(baseConfig);
  await app.ready();

  const first = await app.inject({ method: 'GET', url: '/version', headers: { origin: 'http://localhost' } });
  assert.equal(first.statusCode, 200);
  assert.equal(first.headers['access-control-allow-origin'], 'http://localhost');
  assert.equal(first.headers['access-control-allow-credentials'], 'true');
  assert.ok(String(first.headers['access-control-expose-headers'] ?? '').toLowerCase().includes('content-length'));

  const second = await app.inject({ method: 'GET', url: '/version', headers: { origin: 'http://localhost' } });
  assert.equal(second.statusCode, 429);
  assert.equal(second.headers['access-control-allow-origin'], 'http://localhost');
  assert.equal(second.headers['access-control-allow-credentials'], 'true');
  assert.ok(String(second.headers['access-control-expose-headers'] ?? '').toLowerCase().includes('content-length'));

  await app.close();
});

test('rate limiter returns application/dns-message for /dns-query', async () => {
  const { app } = buildServer(baseConfig);
  await app.ready();

  // Consume the 1 req/min budget.
  const first = await app.inject({ method: 'GET', url: '/version', headers: { origin: 'http://localhost' } });
  assert.equal(first.statusCode, 200);

  const dns = 'AAABAAABAAAAAAAAB2V4YW1wbGUDY29tAAABAAE'; // example.com A, RFC8484 base64url
  const limited = await app.inject({ method: 'GET', url: `/dns-query?dns=${dns}`, headers: { origin: 'http://localhost' } });
  assert.equal(limited.statusCode, 429);
  assert.ok(String(limited.headers['content-type'] ?? '').startsWith('application/dns-message'));
  assert.equal(String(limited.headers['cache-control'] ?? ''), 'no-store');
  assert.ok(String(limited.headers['access-control-expose-headers'] ?? '').toLowerCase().includes('content-length'));
  const header = decodeDnsHeader(limited.rawPayload);
  assert.equal(header.id, 0);
  // RCODE=2 (SERVFAIL)
  assert.equal(header.flags & 0x000f, 2);

  await app.close();
});

test('rate limiter keys off X-Forwarded-For when TRUST_PROXY=1', async () => {
  const { app } = buildServer({ ...baseConfig, TRUST_PROXY: true });
  await app.ready();

  const first = await app.inject({
    method: 'GET',
    url: '/version',
    headers: { 'x-forwarded-for': '203.0.113.1' },
    remoteAddress: '127.0.0.1',
  });
  assert.equal(first.statusCode, 200);

  // Different forwarded IP should be treated as a different bucket.
  const second = await app.inject({
    method: 'GET',
    url: '/version',
    headers: { 'x-forwarded-for': '203.0.113.2' },
    remoteAddress: '127.0.0.1',
  });
  assert.equal(second.statusCode, 200);

  // Same forwarded IP should hit the limit.
  const third = await app.inject({
    method: 'GET',
    url: '/version',
    headers: { 'x-forwarded-for': '203.0.113.1' },
    remoteAddress: '127.0.0.1',
  });
  assert.equal(third.statusCode, 429);

  await app.close();
});

test('rate limiter does not apply to /healthz, /readyz, or /metrics (including base-path variants)', async () => {
  const { app } = buildServer({ ...baseConfig, PUBLIC_BASE_URL: 'http://localhost/base' });
  await app.ready();

  // Consume the 1 req/min budget.
  const first = await app.inject({ method: 'GET', url: '/base/version', headers: { origin: 'http://localhost' } });
  assert.equal(first.statusCode, 200);

  const limited = await app.inject({ method: 'GET', url: '/base/version', headers: { origin: 'http://localhost' } });
  assert.equal(limited.statusCode, 429);

  // Exempt operational endpoints should still respond.
  const health = await app.inject({ method: 'GET', url: '/base/healthz' });
  assert.equal(health.statusCode, 200);

  const ready = await app.inject({ method: 'GET', url: '/base/readyz' });
  assert.equal(ready.statusCode, 200);

  const metrics = await app.inject({ method: 'GET', url: '/base/metrics' });
  assert.equal(metrics.statusCode, 200);

  await app.close();
});
