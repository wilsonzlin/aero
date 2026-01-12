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
  SESSION_SECRET: 'test-secret',
  SESSION_TTL_SECONDS: 60 * 60 * 24,
  SESSION_COOKIE_SAMESITE: 'Lax' as const,
  RATE_LIMIT_REQUESTS_PER_MINUTE: 0,
  TLS_ENABLED: false,
  TLS_CERT_PATH: '',
  TLS_KEY_PATH: '',
  TCP_ALLOW_PRIVATE_IPS: false,
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

  UDP_RELAY_BASE_URL: '',
  UDP_RELAY_AUTH_MODE: 'none' as const,
  UDP_RELAY_API_KEY: '',
  UDP_RELAY_JWT_SECRET: '',
  UDP_RELAY_TOKEN_TTL_SECONDS: 300,
  UDP_RELAY_AUDIENCE: '',
  UDP_RELAY_ISSUER: '',
};

test('GET /healthz returns ok', async () => {
  const { app } = buildServer(baseConfig);
  await app.ready();

  const response = await app.inject({ method: 'GET', url: '/healthz' });
  assert.equal(response.statusCode, 200);
  assert.deepEqual(JSON.parse(response.body), { ok: true });

  await app.close();
});
