import assert from 'node:assert/strict';
import { createHmac } from 'node:crypto';
import test from 'node:test';
import { buildServer } from '../src/server.js';
import { makeTestConfig } from './testConfig.js';

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
  const payloadRaw = JSON.parse(decodeBase64Url(payloadPart).toString('utf8')) as unknown;
  assert.ok(payloadRaw && typeof payloadRaw === 'object', 'expected session token payload object');
  const sid = (payloadRaw as Record<string, unknown>).sid;
  assert.ok(typeof sid === 'string' && sid.length > 0, 'expected sid in session token');
  return { cookieValue, sessionId: sid };
}

const baseConfig = makeTestConfig({
  TCP_ALLOW_PRIVATE_IPS: false,
  TCP_PROXY_MAX_CONNECTIONS: 64,
  DNS_ALLOW_ANY: false,
  DNS_ALLOW_PRIVATE_PTR: false,
});

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
  const body = JSON.parse(res.body) as unknown;
  assert.ok(body && typeof body === 'object', 'expected JSON object response');
  assert.deepEqual((body as Record<string, unknown>).udpRelay, {
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
  for (const baseUrl of ['ws://relay.example.com', 'wss://relay.example.com'] as const) {
    const { app } = buildServer({
      ...baseConfig,
      UDP_RELAY_BASE_URL: baseUrl,
      UDP_RELAY_AUTH_MODE: 'none',
    });
    await app.ready();

    const res = await app.inject({ method: 'POST', url: '/session', headers: { origin: 'http://localhost' } });
    assert.equal(res.statusCode, 201);
    const body = JSON.parse(res.body) as unknown;
    assert.ok(body && typeof body === 'object', 'expected JSON object response');
    const udpRelay = (body as Record<string, unknown>).udpRelay;
    assert.ok(udpRelay && typeof udpRelay === 'object', 'expected udpRelay object');
    assert.equal((udpRelay as Record<string, unknown>).baseUrl, baseUrl);

    await app.close();
  }
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
  const body = JSON.parse(res.body) as unknown;
  assert.ok(body && typeof body === 'object', 'expected JSON object response');
  const udpRelay = (body as Record<string, unknown>).udpRelay;
  assert.ok(udpRelay && typeof udpRelay === 'object', 'expected udpRelay object');
  const udpRelayRec = udpRelay as Record<string, unknown>;
  assert.equal(udpRelayRec.authMode, 'api_key');
  assert.equal(udpRelayRec.token, 'dev-key');
  assert.ok(typeof udpRelayRec.expiresAt === 'string');

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

  const body = JSON.parse(res.body) as unknown;
  assert.ok(body && typeof body === 'object', 'expected JSON object response');
  const udpRelay = (body as Record<string, unknown>).udpRelay;
  assert.ok(udpRelay && typeof udpRelay === 'object', 'expected udpRelay object');
  const token = (udpRelay as Record<string, unknown>).token as string;
  assert.ok(typeof token === 'string' && token.length > 0, 'expected udpRelay.token');

  const parts = token.split('.');
  assert.equal(parts.length, 3);
  const [headerPart, payloadPart, signaturePart] = parts;
  assert.ok(headerPart && payloadPart && signaturePart);

  const payload = JSON.parse(decodeBase64Url(payloadPart).toString('utf8')) as unknown;
  assert.ok(payload && typeof payload === 'object', 'expected JWT payload object');
  const payloadRec = payload as Record<string, unknown>;
  assert.equal(payloadRec.sid, sessionId);
  assert.equal(payloadRec.origin, 'http://localhost');
  assert.equal(payloadRec.aud, 'aero-udp-relay');
  assert.equal(payloadRec.iss, 'aero-gateway');
  assert.equal((payloadRec.exp as number) - (payloadRec.iat as number), ttlSeconds);

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
