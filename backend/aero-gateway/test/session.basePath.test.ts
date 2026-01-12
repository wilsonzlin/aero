import assert from 'node:assert/strict';
import test from 'node:test';
import { buildServer } from '../src/server.js';

const baseConfig = {
  HOST: '127.0.0.1',
  PORT: 0,
  LOG_LEVEL: 'silent' as const,
  ALLOWED_ORIGINS: ['http://localhost'],
  PUBLIC_BASE_URL: 'https://gateway.example.com/base/',
  SHUTDOWN_GRACE_MS: 100,
  CROSS_ORIGIN_ISOLATION: false,
  TRUST_PROXY: false,
  SESSION_SECRET: 'test-secret',
  SESSION_TTL_SECONDS: 60 * 60 * 24,
  SESSION_COOKIE_SAMESITE: 'Lax' as const,
  RATE_LIMIT_REQUESTS_PER_MINUTE: 0,
  TLS_ENABLED: false,
  TLS_CERT_PATH: '',
  TLS_KEY_PATH: '',
  TCP_ALLOWED_HOSTS: [],
  TCP_ALLOWED_PORTS: [],
  TCP_BLOCKED_CLIENT_IPS: [],
  TCP_MUX_MAX_STREAMS: 1024,
  TCP_MUX_MAX_STREAM_BUFFER_BYTES: 1024 * 1024,
  TCP_MUX_MAX_FRAME_PAYLOAD_BYTES: 16 * 1024 * 1024,
  TCP_PROXY_MAX_CONNECTIONS: 64,
  TCP_PROXY_MAX_CONNECTIONS_PER_IP: 0,
  TCP_PROXY_MAX_MESSAGE_BYTES: 1024 * 1024,
  TCP_PROXY_CONNECT_TIMEOUT_MS: 10_000,
  TCP_PROXY_IDLE_TIMEOUT_MS: 300_000,
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

test('POST /session endpoint discovery includes PUBLIC_BASE_URL base path', async () => {
  const { app } = buildServer(baseConfig);
  await app.ready();

  const res = await app.inject({ method: 'POST', url: '/session' });
  assert.equal(res.statusCode, 201);

  const body = JSON.parse(res.body);
  assert.equal(body?.endpoints?.l2, '/base/l2');
  assert.equal(body?.endpoints?.tcp, '/base/tcp');

  await app.close();
});

