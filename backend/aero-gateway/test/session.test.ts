import assert from 'node:assert/strict';
import test from 'node:test';
import { buildServer } from '../src/server.js';
import {
  L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD_BYTES,
  L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD_BYTES,
} from '../src/protocol/l2Tunnel.js';

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

test('POST /session sets aero_session cookie and returns CreateSessionResponse', async () => {
  const { app } = buildServer(baseConfig);
  await app.ready();

  const res = await app.inject({ method: 'POST', url: '/session' });
  assert.equal(res.statusCode, 201);
  assert.equal(res.headers['cache-control'], 'no-store');

  const setCookie = res.headers['set-cookie'];
  assert.ok(setCookie, 'expected Set-Cookie header');
  const cookieHeader = Array.isArray(setCookie) ? setCookie.join('; ') : setCookie;
  assert.match(cookieHeader, /\baero_session=/);
  assert.match(cookieHeader, /\bHttpOnly\b/);
  assert.match(cookieHeader, /\bPath=\//);
  assert.match(cookieHeader, /\bSameSite=Lax\b/);
  assert.ok(!/\bSecure\b/.test(cookieHeader), 'unexpected Secure attribute for non-secure request');

  const body = JSON.parse(res.body);
  assert.equal(typeof body?.session?.expiresAt, 'string');
  assert.ok(!Number.isNaN(Date.parse(body.session.expiresAt)));
  assert.deepEqual(body.endpoints, {
    tcp: '/tcp',
    dnsQuery: '/dns-query',
    tcpMux: '/tcp-mux',
    dnsJson: '/dns-json',
    l2: '/l2',
    udpRelayToken: '/udp-relay/token',
  });
  assert.deepEqual(body.limits?.dns, { maxQueryBytes: 4096 });
  assert.deepEqual(body.limits?.l2, {
    maxFramePayloadBytes: L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD_BYTES,
    maxControlPayloadBytes: L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD_BYTES,
  });
  assert.equal(body.limits?.tcp?.maxConnections, 64);
  assert.equal(body.limits?.tcp?.maxMessageBytes, 1024 * 1024);
  assert.equal(body.limits?.tcp?.connectTimeoutMs, 10_000);
  assert.equal(body.limits?.tcp?.idleTimeoutMs, 300_000);

  await app.close();
});

test('POST /session sets Secure when TRUST_PROXY=1 and X-Forwarded-Proto=https', async () => {
  const { app } = buildServer({ ...baseConfig, TRUST_PROXY: true });
  await app.ready();

  const res = await app.inject({
    method: 'POST',
    url: '/session',
    headers: { 'x-forwarded-proto': 'https' },
  });
  assert.equal(res.statusCode, 201);

  const setCookie = res.headers['set-cookie'];
  assert.ok(setCookie, 'expected Set-Cookie header');
  const cookieHeader = Array.isArray(setCookie) ? setCookie.join('; ') : setCookie;
  assert.match(cookieHeader, /\bSecure\b/);

  await app.close();
});
