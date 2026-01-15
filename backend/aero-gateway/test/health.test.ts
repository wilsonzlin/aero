import assert from 'node:assert/strict';
import test from 'node:test';
import { buildServer } from '../src/server.js';
import { makeTestConfig } from './testConfig.js';

const baseConfig = makeTestConfig({
  TCP_ALLOW_PRIVATE_IPS: false,
  TCP_PROXY_MAX_CONNECTIONS: 64,
  DNS_ALLOW_ANY: false,
  DNS_ALLOW_PRIVATE_PTR: false,
});

test('GET /healthz returns ok', async () => {
  const { app } = buildServer(baseConfig);
  await app.ready();

  const response = await app.inject({ method: 'GET', url: '/healthz' });
  assert.equal(response.statusCode, 200);
  assert.deepEqual(JSON.parse(response.body), { ok: true });

  await app.close();
});

test('GET /healthz rejects overly long request URLs (414)', async () => {
  const { app } = buildServer(baseConfig);
  await app.ready();

  const response = await app.inject({ method: 'GET', url: `/healthz?${'a'.repeat(9000)}` });
  assert.equal(response.statusCode, 414);
  assert.deepEqual(JSON.parse(response.body), { error: 'url_too_long', message: 'Request URL too long' });

  await app.close();
});
