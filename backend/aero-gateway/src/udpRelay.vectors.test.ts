import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import type { Config } from './config.js';
import { mintUdpRelayToken } from './udpRelay.js';

type VectorsFile = {
  version: number;
  'aero-udp-relay-jwt-hs256': {
    secret: string;
    ttlSeconds: number;
    nowUnix: number;
    tokens: {
      valid: {
        token: string;
        claims: { sid: string; origin: string; iat: number; exp: number; aud: string; iss: string };
      };
    };
  };
};

function loadVectors(): VectorsFile {
  const here = path.dirname(fileURLToPath(import.meta.url));
  const vectorsPath = path.join(here, '../../../crates/conformance/test-vectors/aero-vectors-v1.json');
  return JSON.parse(readFileSync(vectorsPath, 'utf8')) as VectorsFile;
}

test('UDP relay HS256 JWT matches canonical vectors', () => {
  const vectors = loadVectors();
  assert.equal(vectors.version, 1, 'unexpected vector file version');

  const jwt = vectors['aero-udp-relay-jwt-hs256'];

  const cfg = {
    UDP_RELAY_BASE_URL: 'https://relay.example.test',
    UDP_RELAY_AUTH_MODE: 'jwt',
    UDP_RELAY_API_KEY: '',
    UDP_RELAY_JWT_SECRET: jwt.secret,
    UDP_RELAY_TOKEN_TTL_SECONDS: jwt.ttlSeconds,
    UDP_RELAY_AUDIENCE: jwt.tokens.valid.claims.aud,
    UDP_RELAY_ISSUER: jwt.tokens.valid.claims.iss,
  } as unknown as Config;

  const tokenInfo = mintUdpRelayToken(cfg, {
    sessionId: jwt.tokens.valid.claims.sid,
    origin: jwt.tokens.valid.claims.origin,
    nowMs: jwt.nowUnix * 1000,
  });
  assert.ok(tokenInfo, 'expected mintUdpRelayToken() to return token');
  assert.equal(tokenInfo.authMode, 'jwt');
  assert.equal(tokenInfo.token, jwt.tokens.valid.token);
});

