import assert from 'node:assert/strict';
import net from 'node:net';
import test from 'node:test';
import WebSocket from 'ws';
import { buildServer } from '../src/server.js';

async function listenNet(server: net.Server): Promise<number> {
  await new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => resolve());
  });
  const addr = server.address();
  if (!addr || typeof addr === 'string') throw new Error('Expected TCP address');
  return addr.port;
}

async function listenGateway(app: import('fastify').FastifyInstance): Promise<number> {
  await app.listen({ host: '127.0.0.1', port: 0 });
  const addr = app.server.address();
  if (!addr || typeof addr === 'string') throw new Error('Expected TCP address');
  return addr.port;
}

async function createSessionCookie(baseUrl: string): Promise<string> {
  const res = await fetch(`${baseUrl}/session`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({}),
  });
  if (!res.ok) throw new Error(`Failed to create session: ${res.status}`);
  const setCookie = res.headers.get('set-cookie');
  if (!setCookie) throw new Error('Missing Set-Cookie header');
  return setCookie.split(';')[0] ?? setCookie;
}

async function expectWsRejected(
  url: string,
  init: WebSocket.ClientOptions,
  expectedStatus: number,
  protocols?: string,
): Promise<void> {
  const status = await new Promise<number>((resolve, reject) => {
    const ws = protocols ? new WebSocket(url, protocols, init) : new WebSocket(url, init);
    let settled = false;

    ws.once('unexpected-response', (_req, res) => {
      settled = true;
      res.resume();
      resolve(res.statusCode ?? 0);
      ws.terminate();
    });
    ws.once('open', () => {
      if (settled) return;
      settled = true;
      ws.terminate();
      reject(new Error('WebSocket unexpectedly opened'));
    });
    ws.once('error', (err) => {
      if (settled) return;
      settled = true;
      reject(err);
    });
  });
  assert.equal(status, expectedStatus);
}

test('WebSocket upgrades work under the PUBLIC_BASE_URL base path prefix', async () => {
  const originalAllowPrivate = process.env.TCP_ALLOW_PRIVATE_IPS;
  process.env.TCP_ALLOW_PRIVATE_IPS = '1';

  const echoServer = net.createServer((socket) => socket.on('data', (data) => socket.write(data)));
  const echoPort = await listenNet(echoServer);

  const { app } = buildServer({
    HOST: '127.0.0.1',
    PORT: 0,
    LOG_LEVEL: 'silent',
    ALLOWED_ORIGINS: ['http://localhost'],
    PUBLIC_BASE_URL: 'http://localhost/base',
    SHUTDOWN_GRACE_MS: 100,
    CROSS_ORIGIN_ISOLATION: false,
    TRUST_PROXY: false,
    SESSION_SECRET: 'test-secret',
    SESSION_TTL_SECONDS: 60 * 60 * 24,
    SESSION_COOKIE_SAMESITE: 'Lax',
    RATE_LIMIT_REQUESTS_PER_MINUTE: 0,
    TLS_ENABLED: false,
    TLS_CERT_PATH: '',
    TLS_KEY_PATH: '',
    TCP_ALLOW_PRIVATE_IPS: true,
    TCP_ALLOWED_HOSTS: [],
    TCP_ALLOWED_PORTS: [],
    TCP_BLOCKED_CLIENT_IPS: [],
    TCP_MUX_MAX_STREAMS: 1024,
    TCP_MUX_MAX_STREAM_BUFFER_BYTES: 1024 * 1024,
    TCP_MUX_MAX_FRAME_PAYLOAD_BYTES: 16 * 1024 * 1024,
    TCP_PROXY_MAX_CONNECTIONS: 1,
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
    DNS_ALLOW_ANY: true,
    DNS_ALLOW_PRIVATE_PTR: true,
    DNS_QPS_PER_IP: 0,
    DNS_BURST_PER_IP: 0,

    UDP_RELAY_BASE_URL: '',
    UDP_RELAY_AUTH_MODE: 'none',
    UDP_RELAY_API_KEY: '',
    UDP_RELAY_JWT_SECRET: '',
    UDP_RELAY_TOKEN_TTL_SECONDS: 300,
    UDP_RELAY_AUDIENCE: '',
    UDP_RELAY_ISSUER: '',
  });

  await app.ready();
  const port = await listenGateway(app);
  const baseUrl = `http://127.0.0.1:${port}/base`;
  const wsBase = `ws://127.0.0.1:${port}/base`;

  try {
    await expectWsRejected(`${wsBase}/tcp?v=1&host=127.0.0.1&port=${echoPort}`, {}, 401);
    await expectWsRejected(`${wsBase}/tcp-mux`, {}, 401, 'aero-tcp-mux-v1');

    const cookie = await createSessionCookie(baseUrl);

    const ws1 = await new Promise<WebSocket>((resolve, reject) => {
      const ws = new WebSocket(`${wsBase}/tcp?v=1&host=127.0.0.1&port=${echoPort}`, { headers: { cookie } });
      ws.once('open', () => resolve(ws));
      ws.once('error', reject);
    });

    const payload = Buffer.from('ping', 'utf8');
    const echoed = await new Promise<Buffer>((resolve, reject) => {
      ws1.once('message', (data) => resolve(Buffer.isBuffer(data) ? data : Buffer.from(data as ArrayBuffer)));
      ws1.once('error', reject);
      ws1.send(payload);
    });
    assert.deepEqual(echoed, payload);

    // Second connection should be rejected due to per-session maxConnections=1.
    await expectWsRejected(`${wsBase}/tcp?v=1&host=127.0.0.1&port=${echoPort}`, { headers: { cookie } }, 429);

    ws1.close();
    await new Promise<void>((resolve) => ws1.once('close', () => resolve()));

    const mux = await new Promise<WebSocket>((resolve, reject) => {
      const ws = new WebSocket(`${wsBase}/tcp-mux`, 'aero-tcp-mux-v1', { headers: { cookie } });
      ws.once('open', () => resolve(ws));
      ws.once('error', reject);
    });
    mux.close();
    await new Promise<void>((resolve) => mux.once('close', () => resolve()));
  } finally {
    await app.close();
    echoServer.close();
    process.env.TCP_ALLOW_PRIVATE_IPS = originalAllowPrivate;
  }
});

