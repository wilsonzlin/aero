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
  RATE_LIMIT_REQUESTS_PER_MINUTE: 1,
  TCP_PROXY_MAX_CONNECTIONS: 0,
  TCP_PROXY_MAX_CONNECTIONS_PER_IP: 0,
  DNS_UPSTREAMS: [],
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

