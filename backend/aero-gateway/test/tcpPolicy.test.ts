import assert from 'node:assert/strict';
import type http from 'node:http';
import test from 'node:test';

import { validateWsUpgradePolicy } from '../src/routes/tcpPolicy.js';
import { validateWebSocketHandshakeRequest } from '../src/routes/wsUpgradeRequest.js';

test('validateWsUpgradePolicy rejects multiple Origin header values', () => {
  const req = {
    headers: { origin: ['https://allowed.example.com', 'https://blocked.example.com'] },
    socket: { remoteAddress: '127.0.0.1' },
  } as unknown as http.IncomingMessage;

  const decision = validateWsUpgradePolicy(req, { allowedOrigins: ['*'] });
  assert.deepEqual(decision, { ok: false, status: 403, message: 'Origin not allowed' });
});

test('validateWebSocketHandshakeRequest accepts a standard handshake (and trims Sec-WebSocket-Key)', () => {
  const req = {
    headers: {
      upgrade: 'WebSocket',
      connection: 'Upgrade',
      'sec-websocket-version': '13',
      'sec-websocket-key': '  abc  ',
    },
  } as unknown as http.IncomingMessage;

  const decision = validateWebSocketHandshakeRequest(req);
  assert.deepEqual(decision, { ok: true, key: 'abc' });
});

test('validateWebSocketHandshakeRequest rejects oversized Upgrade header values', () => {
  const req = {
    headers: {
      upgrade: `websocket${'x'.repeat(300)}`,
      connection: 'Upgrade',
      'sec-websocket-version': '13',
      'sec-websocket-key': 'abc',
    },
  } as unknown as http.IncomingMessage;

  const decision = validateWebSocketHandshakeRequest(req);
  assert.deepEqual(decision, { ok: false, status: 400, message: 'Invalid WebSocket upgrade' });
});

test('validateWebSocketHandshakeRequest rejects non-13 WebSocket versions', () => {
  const req = {
    headers: {
      upgrade: 'websocket',
      connection: 'Upgrade',
      'sec-websocket-version': '12',
      'sec-websocket-key': 'abc',
    },
  } as unknown as http.IncomingMessage;

  const decision = validateWebSocketHandshakeRequest(req);
  assert.deepEqual(decision, { ok: false, status: 400, message: 'Invalid WebSocket upgrade' });
});

test('validateWebSocketHandshakeRequest rejects repeated handshake headers', () => {
  const req = {
    headers: {
      upgrade: ['websocket', 'websocket'],
      connection: 'Upgrade',
      'sec-websocket-version': '13',
      'sec-websocket-key': 'abc',
    },
  } as unknown as http.IncomingMessage;

  const decision = validateWebSocketHandshakeRequest(req);
  assert.deepEqual(decision, { ok: false, status: 400, message: 'Invalid WebSocket upgrade' });
});

test('validateWebSocketHandshakeRequest rejects repeated handshake headers in rawHeaders', () => {
  const req = {
    headers: {
      upgrade: 'websocket, websocket',
      connection: 'Upgrade',
      'sec-websocket-version': '13',
      'sec-websocket-key': 'abc',
    },
    rawHeaders: [
      'Upgrade',
      'websocket',
      'Upgrade',
      'websocket',
      'Connection',
      'Upgrade',
      'Sec-WebSocket-Version',
      '13',
      'Sec-WebSocket-Key',
      'abc',
    ],
  } as unknown as http.IncomingMessage;

  const decision = validateWebSocketHandshakeRequest(req);
  assert.deepEqual(decision, { ok: false, status: 400, message: 'Invalid WebSocket upgrade' });
});

test('validateWebSocketHandshakeRequest accepts comma-separated token lists for Upgrade/Connection', () => {
  const req = {
    headers: {
      upgrade: 'h2c, WebSocket',
      connection: 'keep-alive, Upgrade',
      'sec-websocket-version': '13',
      'sec-websocket-key': 'abc',
    },
  } as unknown as http.IncomingMessage;

  const decision = validateWebSocketHandshakeRequest(req);
  assert.deepEqual(decision, { ok: true, key: 'abc' });
});

test('validateWebSocketHandshakeRequest rejects partial token matches for Upgrade', () => {
  const req = {
    headers: {
      upgrade: 'websocket2',
      connection: 'Upgrade',
      'sec-websocket-version': '13',
      'sec-websocket-key': 'abc',
    },
  } as unknown as http.IncomingMessage;

  const decision = validateWebSocketHandshakeRequest(req);
  assert.deepEqual(decision, { ok: false, status: 400, message: 'Invalid WebSocket upgrade' });
});

