import assert from 'node:assert/strict';
import { createHmac } from 'node:crypto';
import test from 'node:test';
import { buildServer } from '../src/server.js';

function decodeBase64Url(base64url: string): Buffer {
  if (!/^[A-Za-z0-9_-]+$/.test(base64url)) throw new Error('Invalid base64url');
  let base64 = base64url.replaceAll('-', '+').replaceAll('_', '/');
  const mod = base64.length % 4;
  if (mod === 2) base64 += '==';
  else if (mod === 3) base64 += '=';
  else if (mod !== 0) throw new Error('Invalid base64url length');
  return Buffer.from(base64, 'base64');
}

function encodeBase64Url(buf: Buffer): string {
  return buf.toString('base64').replaceAll('=', '').replaceAll('+', '-').replaceAll('/', '_');
}

function extractSessionCookie(setCookie: string | string[] | undefined): { cookieValue: string; sessionId: string } {
  assert.ok(setCookie, 'expected Set-Cookie header');
  const cookie = Array.isArray(setCookie) ? setCookie[0] : setCookie;
  const m = cookie.match(/^aero_session=([^;]+)/);
  assert.ok(m, 'expected aero_session cookie');
  const cookieValue = decodeURIComponent(m[1] ?? '');
  const tokenParts = cookieValue.split('.');
  assert.equal(tokenParts.length, 2, 'expected session token payload.sig');
  const [payloadPart] = tokenParts;
  const payload = JSON.parse(decodeBase64Url(payloadPart).toString('utf8')) as any;
  assert.ok(payload && typeof payload.sid === 'string' && payload.sid.length > 0, 'expected sid in session token');
  return { cookieValue, sessionId: payload.sid };
}

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

test('POST /session omits udpRelay when UDP_RELAY_BASE_URL is unset', async () => {
  const { app } = buildServer(baseConfig);
  await app.ready();

  const res = await app.inject({ method: 'POST', url: '/session', headers: { origin: 'http://localhost' } });
  assert.equal(res.statusCode, 201);
  const body = JSON.parse(res.body) as Record<string, unknown>;
  assert.ok(!('udpRelay' in body));

  await app.close();
});

test('POST /session includes udpRelay endpoints when configured (auth_mode=none)', async () => {
  const { app } = buildServer({
    ...baseConfig,
    UDP_RELAY_BASE_URL: 'https://relay.example.com',
    UDP_RELAY_AUTH_MODE: 'none',
  });
  await app.ready();

  const res = await app.inject({ method: 'POST', url: '/session', headers: { origin: 'http://localhost' } });
  assert.equal(res.statusCode, 201);
  const body = JSON.parse(res.body) as any;
  assert.deepEqual(body.udpRelay, {
    baseUrl: 'https://relay.example.com',
    authMode: 'none',
    endpoints: {
      webrtcSignal: '/webrtc/signal',
      webrtcOffer: '/webrtc/offer',
      udp: '/udp',
      webrtcIce: '/webrtc/ice',
    },
  });

  await app.close();
});

test('POST /session preserves ws(s) UDP_RELAY_BASE_URL schemes in udpRelay.baseUrl', async () => {
  const { app } = buildServer({
    ...baseConfig,
    UDP_RELAY_BASE_URL: 'wss://relay.example.com',
    UDP_RELAY_AUTH_MODE: 'none',
  });
  await app.ready();

  const res = await app.inject({ method: 'POST', url: '/session', headers: { origin: 'http://localhost' } });
  assert.equal(res.statusCode, 201);
  const body = JSON.parse(res.body) as any;
  assert.equal(body.udpRelay.baseUrl, 'wss://relay.example.com');

  await app.close();
});

test('POST /session includes api_key token when configured', async () => {
  const { app } = buildServer({
    ...baseConfig,
    UDP_RELAY_BASE_URL: 'https://relay.example.com',
    UDP_RELAY_AUTH_MODE: 'api_key',
    UDP_RELAY_API_KEY: 'dev-key',
    UDP_RELAY_TOKEN_TTL_SECONDS: 60,
  });
  await app.ready();

  const res = await app.inject({ method: 'POST', url: '/session', headers: { origin: 'http://localhost' } });
  assert.equal(res.statusCode, 201);
  const body = JSON.parse(res.body) as any;
  assert.equal(body.udpRelay.authMode, 'api_key');
  assert.equal(body.udpRelay.token, 'dev-key');
  assert.ok(typeof body.udpRelay.expiresAt === 'string');

  await app.close();
});

test('POST /session mints a short-lived UDP relay JWT token bound to session id', async () => {
  const ttlSeconds = 123;
  const secret = 'test-secret';

  const { app } = buildServer({
    ...baseConfig,
    UDP_RELAY_BASE_URL: 'https://relay.example.com',
    UDP_RELAY_AUTH_MODE: 'jwt',
    UDP_RELAY_JWT_SECRET: secret,
    UDP_RELAY_TOKEN_TTL_SECONDS: ttlSeconds,
    UDP_RELAY_AUDIENCE: 'aero-udp-relay',
    UDP_RELAY_ISSUER: 'aero-gateway',
  });
  await app.ready();

  const res = await app.inject({ method: 'POST', url: '/session', headers: { origin: 'http://localhost' } });
  assert.equal(res.statusCode, 201);
  const { sessionId } = extractSessionCookie(res.headers['set-cookie']);

  const body = JSON.parse(res.body) as any;
  const token = body.udpRelay.token as string;
  assert.ok(typeof token === 'string' && token.length > 0, 'expected udpRelay.token');

  const parts = token.split('.');
  assert.equal(parts.length, 3);
  const [headerPart, payloadPart, signaturePart] = parts;
  assert.ok(headerPart && payloadPart && signaturePart);

  const payload = JSON.parse(decodeBase64Url(payloadPart).toString('utf8')) as any;
  assert.equal(payload.sid, sessionId);
  assert.equal(payload.origin, 'http://localhost');
  assert.equal(payload.aud, 'aero-udp-relay');
  assert.equal(payload.iss, 'aero-gateway');
  assert.equal(payload.exp - payload.iat, ttlSeconds);

  const expectedSig = encodeBase64Url(createHmac('sha256', secret).update(`${headerPart}.${payloadPart}`).digest());
  assert.equal(signaturePart, expectedSig);

  await app.close();
});

test('POST /udp-relay/token enforces session cookie + rate limiting', async () => {
  const { app } = buildServer({
    ...baseConfig,
    UDP_RELAY_BASE_URL: 'https://relay.example.com',
    UDP_RELAY_AUTH_MODE: 'jwt',
    UDP_RELAY_JWT_SECRET: 'secret',
  });
  await app.ready();

  // Missing Origin header.
  const missingOrigin = await app.inject({ method: 'POST', url: '/udp-relay/token' });
  assert.equal(missingOrigin.statusCode, 403);

  // Missing cookie.
  const missingCookie = await app.inject({
    method: 'POST',
    url: '/udp-relay/token',
    headers: { origin: 'http://localhost' },
  });
  assert.equal(missingCookie.statusCode, 401);

  // Create session and capture cookie.
  const session = await app.inject({ method: 'POST', url: '/session', headers: { origin: 'http://localhost' } });
  const { cookieValue } = extractSessionCookie(session.headers['set-cookie']);
  const cookie = `aero_session=${encodeURIComponent(cookieValue)}`;

  // Burst is 5, so 6th request should be rejected.
  for (let i = 0; i < 5; i++) {
    const ok = await app.inject({
      method: 'POST',
      url: '/udp-relay/token',
      headers: { origin: 'http://localhost', cookie },
    });
    assert.equal(ok.statusCode, 200);
  }

  const limited = await app.inject({
    method: 'POST',
    url: '/udp-relay/token',
    headers: { origin: 'http://localhost', cookie },
  });
  assert.equal(limited.statusCode, 429);

  await app.close();
});
