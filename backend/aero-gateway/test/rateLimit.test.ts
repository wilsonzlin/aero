import assert from 'node:assert/strict';
import test from 'node:test';
import { buildServer } from '../src/server.js';

const baseConfig = {
  HOST: '127.0.0.1',
  PORT: 0,
  LOG_LEVEL: 'silent' as const,
  ALLOWED_ORIGINS: ['http://localhost'],
  PUBLIC_BASE_URL: 'http://localhost',
  SHUTDOWN_GRACE_MS: 100,
  CROSS_ORIGIN_ISOLATION: false,
  TRUST_PROXY: false,
  RATE_LIMIT_REQUESTS_PER_MINUTE: 1,
  TLS_ENABLED: false,
  TLS_CERT_PATH: '',
  TLS_KEY_PATH: '',
  TCP_ALLOWED_HOSTS: [],
  TCP_ALLOWED_PORTS: [],
  TCP_BLOCKED_CLIENT_IPS: [],
  TCP_MUX_MAX_STREAMS: 1024,
  TCP_MUX_MAX_STREAM_BUFFER_BYTES: 1024 * 1024,
  TCP_MUX_MAX_FRAME_PAYLOAD_BYTES: 16 * 1024 * 1024,
  TCP_PROXY_MAX_CONNECTIONS: 0,
  TCP_PROXY_MAX_CONNECTIONS_PER_IP: 0,
  DNS_UPSTREAMS: [],
  DNS_UPSTREAM_TIMEOUT_MS: 200,
  DNS_CACHE_MAX_ENTRIES: 0,
  DNS_CACHE_MAX_TTL_SECONDS: 0,
  DNS_CACHE_NEGATIVE_TTL_SECONDS: 0,
  DNS_MAX_QUERY_BYTES: 4096,
  DNS_MAX_RESPONSE_BYTES: 4096,
  DNS_ALLOW_ANY: false,
  DNS_ALLOW_PRIVATE_PTR: false,
  DNS_QPS_PER_IP: 0,
  DNS_BURST_PER_IP: 0,
};

test('rate limiter rejects when the per-minute budget is exceeded', async () => {
  const { app } = buildServer(baseConfig);
  await app.ready();

  const first = await app.inject({ method: 'GET', url: '/version' });
  assert.equal(first.statusCode, 200);

  const second = await app.inject({ method: 'GET', url: '/version' });
  assert.equal(second.statusCode, 429);

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
