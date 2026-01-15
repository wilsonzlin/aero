import assert from 'node:assert/strict';
import net from 'node:net';
import test from 'node:test';
import WebSocket from 'ws';
import { buildServer } from '../src/server.js';
import { makeTestConfig } from './testConfig.js';

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

async function createSessionCookie(app: import('fastify').FastifyInstance): Promise<string> {
  const res = await app.inject({ method: 'POST', url: '/base/session' });
  if (res.statusCode !== 201) throw new Error(`Failed to create session: ${res.statusCode}`);
  const setCookie = res.headers['set-cookie'];
  const raw = Array.isArray(setCookie) ? setCookie[0] : setCookie;
  if (!raw) throw new Error('Missing Set-Cookie header');
  return raw.split(';')[0] ?? raw;
}

async function expectWsRejected(url: string, init: WebSocket.ClientOptions, expectedStatus: number, protocols?: string): Promise<void> {
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
  const echoServer = net.createServer((socket) => socket.on('data', (data) => socket.write(data)));
  const echoPort = await listenNet(echoServer);

  const { app } = buildServer(makeTestConfig({ PUBLIC_BASE_URL: 'http://localhost/base' }));

  await app.ready();
  const port = await listenGateway(app);
  const wsBase = `ws://127.0.0.1:${port}`;

  try {
    await expectWsRejected(`${wsBase}/base/tcp?v=1&host=127.0.0.1&port=${echoPort}`, {}, 401);
    await expectWsRejected(`${wsBase}/base/tcp-mux`, {}, 401, 'aero-tcp-mux-v1');

    const cookie = await createSessionCookie(app);

    const ws1 = await new Promise<WebSocket>((resolve, reject) => {
      const ws = new WebSocket(`${wsBase}/base/tcp?v=1&host=127.0.0.1&port=${echoPort}`, { headers: { cookie } });
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
    await expectWsRejected(`${wsBase}/base/tcp?v=1&host=127.0.0.1&port=${echoPort}`, { headers: { cookie } }, 429);

    ws1.close();
    await new Promise<void>((resolve) => ws1.once('close', () => resolve()));

    const mux = await new Promise<WebSocket>((resolve, reject) => {
      const ws = new WebSocket(`${wsBase}/base/tcp-mux`, 'aero-tcp-mux-v1', { headers: { cookie } });
      ws.once('open', () => resolve(ws));
      ws.once('error', reject);
    });
    mux.close();
    await new Promise<void>((resolve) => mux.once('close', () => resolve()));
  } finally {
    await app.close();
    echoServer.close();
  }
});

