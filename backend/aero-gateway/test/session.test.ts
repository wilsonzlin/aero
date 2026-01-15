import assert from 'node:assert/strict';
import test from 'node:test';
import { buildServer } from '../src/server.js';
import { makeTestConfig } from './testConfig.js';
import {
  L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD_BYTES,
  L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD_BYTES,
} from '../src/protocol/l2Tunnel.js';

const baseConfig = makeTestConfig({
  TCP_ALLOW_PRIVATE_IPS: false,
  TCP_PROXY_MAX_CONNECTIONS: 64,
  DNS_ALLOW_ANY: false,
  DNS_ALLOW_PRIVATE_PTR: false,
});

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

test('POST /session surfaces custom l2 payload limits when configured', async () => {
  const { app } = buildServer({
    ...baseConfig,
    L2_MAX_FRAME_PAYLOAD_BYTES: 1234,
    L2_MAX_CONTROL_PAYLOAD_BYTES: 99,
  });
  await app.ready();

  const res = await app.inject({ method: 'POST', url: '/session' });
  assert.equal(res.statusCode, 201);
  const body = JSON.parse(res.body);
  assert.deepEqual(body.limits?.l2, { maxFramePayloadBytes: 1234, maxControlPayloadBytes: 99 });

  await app.close();
});
