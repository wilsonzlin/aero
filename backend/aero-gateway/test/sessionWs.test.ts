import assert from 'node:assert/strict';
import type http from 'node:http';
import net from 'node:net';
import test from 'node:test';
import WebSocket from 'ws';
import { PassThrough } from 'node:stream';
import { once } from 'node:events';
import { buildServer } from '../src/server.js';
import { makeTestConfig, TEST_WS_HANDSHAKE_HEADERS } from './testConfig.js';

async function captureUpgradeResponse(
  app: import('fastify').FastifyInstance,
  req: http.IncomingMessage,
): Promise<string> {
  const socket = new PassThrough();
  const chunks: Buffer[] = [];
  socket.on('data', (chunk) => chunks.push(Buffer.from(chunk)));
  const ended = once(socket, 'end');
  app.server.emit('upgrade', req, socket, Buffer.alloc(0));
  await ended;
  return Buffer.concat(chunks).toString('utf8');
}

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
  const res = await app.inject({ method: 'POST', url: '/session' });
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

test('server upgrade routing validates WebSocket handshake before auth', async () => {
  const { app } = buildServer(makeTestConfig());

  await app.ready();
  try {
    const reqInvalid = {
      url: '/tcp?v=1&host=127.0.0.1&port=1',
      headers: {},
    } as unknown as http.IncomingMessage;
    const resInvalid = await captureUpgradeResponse(app, reqInvalid);
    assert.ok(resInvalid.startsWith('HTTP/1.1 400 '));
    assert.ok(resInvalid.includes('Invalid WebSocket upgrade'));

    const reqValidHandshakeNoCookie = {
      url: '/tcp?v=1&host=127.0.0.1&port=1',
      headers: {
        ...TEST_WS_HANDSHAKE_HEADERS,
      },
    } as unknown as http.IncomingMessage;
    const resNoCookie = await captureUpgradeResponse(app, reqValidHandshakeNoCookie);
    assert.ok(resNoCookie.startsWith('HTTP/1.1 401 '));

    const reqMuxValidHandshakeNoCookie = {
      url: '/tcp-mux',
      headers: {
        ...TEST_WS_HANDSHAKE_HEADERS,
        'sec-websocket-protocol': 'aero-tcp-mux-v1',
      },
    } as unknown as http.IncomingMessage;
    const resMuxNoCookie = await captureUpgradeResponse(app, reqMuxValidHandshakeNoCookie);
    assert.ok(resMuxNoCookie.startsWith('HTTP/1.1 401 '));
  } finally {
    await app.close();
  }
});

test('server upgrade routing uses rawHeaders order for Cookie (first cookie wins)', async () => {
  const { app } = buildServer(makeTestConfig());

  await app.ready();
  try {
    const cookie = await createSessionCookie(app);

    const req = {
      url: '/tcp?v=1&host=127.0.0.1&port=1',
      headers: {
        ...TEST_WS_HANDSHAKE_HEADERS,
        // If the server incorrectly trusted merged `req.headers.cookie`, this would be accepted.
        cookie,
      },
      // Simulate repeated Cookie headers as they arrive in Node:
      // an earlier empty `aero_session` must "win" and prevent auth bypass.
      rawHeaders: ['Cookie', 'aero_session=', 'Cookie', cookie],
    } as unknown as http.IncomingMessage;

    const res = await captureUpgradeResponse(app, req);
    assert.ok(res.startsWith('HTTP/1.1 401 '));
  } finally {
    await app.close();
  }
});

test('WebSocket upgrades reject missing/invalid session cookies', async () => {
  const echoServer = net.createServer((socket) => socket.on('data', (data) => socket.write(data)));
  const echoPort = await listenNet(echoServer);

  const { app } = buildServer(makeTestConfig());

  await app.ready();
  const port = await listenGateway(app);
  const wsBase = `ws://127.0.0.1:${port}`;

  try {
    await expectWsRejected(`${wsBase}/tcp?v=1&host=127.0.0.1&port=${echoPort}`, {}, 401);
    await expectWsRejected(`${wsBase}/tcp-mux`, {}, 401, 'aero-tcp-mux-v1');

    const cookie = await createSessionCookie(app);

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
  }
});
