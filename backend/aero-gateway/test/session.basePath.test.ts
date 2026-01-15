import assert from 'node:assert/strict';
import test from 'node:test';
import { buildServer } from '../src/server.js';
import { makeTestConfig } from './testConfig.js';

const baseConfig = makeTestConfig({
  PUBLIC_BASE_URL: 'https://gateway.example.com/base/',
  TCP_ALLOW_PRIVATE_IPS: false,
  TCP_PROXY_MAX_CONNECTIONS: 64,
  DNS_ALLOW_ANY: false,
  DNS_ALLOW_PRIVATE_PTR: false,
});

test('POST /session endpoint discovery includes PUBLIC_BASE_URL base path', async () => {
  const { app } = buildServer(baseConfig);
  await app.ready();

  for (const url of ['/session', '/base/session'] as const) {
    const res = await app.inject({ method: 'POST', url });
    assert.equal(res.statusCode, 201);

    const body = JSON.parse(res.body);
    assert.equal(body?.endpoints?.l2, '/base/l2');
    assert.equal(body?.endpoints?.tcp, '/base/tcp');
  }

  await app.close();
});

test('gateway serves HTTP endpoints under PUBLIC_BASE_URL base path without requiring reverse-proxy rewrites', async () => {
  const { app } = buildServer(baseConfig);
  await app.ready();

  const health = await app.inject({ method: 'GET', url: '/base/healthz' });
  assert.equal(health.statusCode, 200);
  assert.deepEqual(JSON.parse(health.body), { ok: true });

  const res = await app.inject({ method: 'POST', url: '/base/session' });
  assert.equal(res.statusCode, 201);

  const body = JSON.parse(res.body);
  assert.equal(body?.endpoints?.l2, '/base/l2');
  assert.equal(body?.endpoints?.tcp, '/base/tcp');

  const metrics = await app.inject({ method: 'GET', url: '/base/metrics' });
  assert.equal(metrics.statusCode, 200);
  assert.match(metrics.body, /http_requests_total/);

  await app.close();
});
