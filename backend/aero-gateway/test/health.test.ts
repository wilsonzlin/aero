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
  RATE_LIMIT_REQUESTS_PER_MINUTE: 0,
  TCP_PROXY_MAX_CONNECTIONS: 0,
  TCP_PROXY_MAX_CONNECTIONS_PER_IP: 0,
  DNS_UPSTREAMS: [],
};

test('GET /healthz returns ok', async () => {
  const { app } = buildServer(baseConfig);
  await app.ready();

  const response = await app.inject({ method: 'GET', url: '/healthz' });
  assert.equal(response.statusCode, 200);
  assert.deepEqual(JSON.parse(response.body), { ok: true });

  await app.close();
});
