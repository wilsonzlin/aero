import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { describe, it } from 'node:test';
import { fileURLToPath } from 'node:url';

import type { Config } from '../src/config.js';
import { mintUdpRelayToken } from '../src/udpRelay.js';

type VectorsFile = {
  schema: number;
  jwtTokens: {
    testSecret: string;
    vectors: Array<
      | {
          name: string;
          secret: string;
          token: string;
          nowSec: number;
          sid: string;
          exp: number;
          iat: number;
          origin?: string;
          aud?: string;
          iss?: string;
        }
      | { name: string; secret: string; token: string; nowSec: number; expectError: true }
    >;
  };
};

type ConformanceVectorsFile = {
  version: number;
  'aero-udp-relay-jwt-hs256': {
    secret: string;
    nowUnix: number;
    ttlSeconds: number;
    tokens: {
      valid: {
        token: string;
        claims: { sid: string; origin: string; iat: number; exp: number; aud: string; iss: string };
      };
      expired: { token: string; claims: { sid: string; origin: string; iat: number; exp: number; aud: string; iss: string } };
      badSignature: { token: string; claims: { sid: string; origin: string; iat: number; exp: number; aud: string; iss: string } };
    };
  };
};

function vectorsPath(): string {
  const dir = path.dirname(fileURLToPath(import.meta.url));
  return path.resolve(dir, '..', '..', '..', 'protocol-vectors', 'auth-tokens.json');
}

function conformanceVectorsPath(): string {
  const dir = path.dirname(fileURLToPath(import.meta.url));
  return path.resolve(dir, '..', '..', '..', 'crates', 'conformance', 'test-vectors', 'aero-vectors-v1.json');
}

describe('UDP relay JWT minting matches vectors', () => {
  const vectors = JSON.parse(fs.readFileSync(vectorsPath(), 'utf8')) as VectorsFile;
  assert.equal(vectors.schema, 1);

  const conformance = JSON.parse(fs.readFileSync(conformanceVectorsPath(), 'utf8')) as ConformanceVectorsFile;
  assert.equal(conformance.version, 1, 'unexpected conformance vector file version');

  it('matches unified conformance vectors', () => {
    const c = conformance['aero-udp-relay-jwt-hs256'];
    assert.equal(c.secret, vectors.jwtTokens.testSecret);

    const byName = new Map(vectors.jwtTokens.vectors.map((v) => [v.name, v] as const));

    const vValid = byName.get('valid');
    assert.ok(vValid && !('expectError' in vValid), 'missing valid jwt vector');
    assert.equal(vValid.token, c.tokens.valid.token);
    assert.equal(vValid.sid, c.tokens.valid.claims.sid);
    assert.equal(vValid.origin, c.tokens.valid.claims.origin);
    assert.equal(vValid.aud, c.tokens.valid.claims.aud);
    assert.equal(vValid.iss, c.tokens.valid.claims.iss);
    assert.equal(vValid.iat, c.tokens.valid.claims.iat);
    assert.equal(vValid.exp, c.tokens.valid.claims.exp);
    assert.equal(vValid.nowSec, c.nowUnix);

    const vExpired = byName.get('expired');
    assert.ok(vExpired, 'missing expired jwt vector');
    assert.equal(vExpired.token, c.tokens.expired.token);

    const vBadSig = byName.get('badSignature');
    assert.ok(vBadSig, 'missing badSignature jwt vector');
    assert.equal(vBadSig.token, c.tokens.badSignature.token);
  });

  for (const v of vectors.jwtTokens.vectors) {
    if ('expectError' in v) continue;
    const ok = v;

    it(ok.name, () => {
      const ttlSeconds = ok.exp - ok.iat;
      const cfg = {
        UDP_RELAY_BASE_URL: 'https://relay.example.test',
        UDP_RELAY_AUTH_MODE: 'jwt',
        UDP_RELAY_API_KEY: '',
        UDP_RELAY_JWT_SECRET: ok.secret,
        UDP_RELAY_TOKEN_TTL_SECONDS: ttlSeconds,
        UDP_RELAY_AUDIENCE: ok.aud ?? '',
        UDP_RELAY_ISSUER: ok.iss ?? '',
      } as unknown as Config;

      const tokenInfo = mintUdpRelayToken(cfg, {
        sessionId: ok.sid,
        origin: ok.origin,
        nowMs: ok.nowSec * 1000,
      });
      assert.ok(tokenInfo, 'expected mintUdpRelayToken to return token');
      assert.equal(tokenInfo.authMode, 'jwt');
      assert.equal(tokenInfo.token, ok.token);
      assert.equal(tokenInfo.expiresAt, new Date(ok.exp * 1000).toISOString());
    });
  }
});
