import assert from 'node:assert/strict';
import type http from 'node:http';
import test from 'node:test';

import { validateWsUpgradePolicy } from '../src/routes/tcpPolicy.js';

test('validateWsUpgradePolicy rejects multiple Origin header values', () => {
  const req = {
    headers: { origin: ['https://allowed.example.com', 'https://blocked.example.com'] },
    socket: { remoteAddress: '127.0.0.1' },
  } as unknown as http.IncomingMessage;

  const decision = validateWsUpgradePolicy(req, { allowedOrigins: ['*'] });
  assert.deepEqual(decision, { ok: false, status: 403, message: 'Origin not allowed' });
});

