import assert from 'node:assert/strict';
import test from 'node:test';

import type { Config } from '../src/config.js';
import { mintUdpRelayToken } from '../src/udpRelay.js';

test('mintUdpRelayToken: throws stable error when JSON.stringify throws', () => {
  const cfg = {
    UDP_RELAY_BASE_URL: 'https://relay.example.test',
    UDP_RELAY_AUTH_MODE: 'jwt',
    UDP_RELAY_API_KEY: '',
    UDP_RELAY_JWT_SECRET: 'secret',
    UDP_RELAY_TOKEN_TTL_SECONDS: 60,
    UDP_RELAY_AUDIENCE: 'aud',
    UDP_RELAY_ISSUER: 'iss',
  } as unknown as Config;

  const original = JSON.stringify;
  try {
    JSON.stringify = () => {
      throw new Error('boom');
    };

    assert.throws(
      () =>
        mintUdpRelayToken(cfg, {
          sessionId: 'sid',
          origin: 'http://localhost',
          nowMs: 1_700_000_000_000,
        }),
      /UDP relay JWT encoding failed/,
    );
  } finally {
    JSON.stringify = original;
  }
});

