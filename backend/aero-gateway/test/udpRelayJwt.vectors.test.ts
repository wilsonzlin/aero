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

function vectorsPath(): string {
  const dir = path.dirname(fileURLToPath(import.meta.url));
  return path.resolve(dir, '..', '..', '..', 'protocol-vectors', 'auth-tokens.json');
}

describe('UDP relay JWT minting matches vectors', () => {
  const vectors = JSON.parse(fs.readFileSync(vectorsPath(), 'utf8')) as VectorsFile;
  assert.equal(vectors.schema, 1);

  for (const v of vectors.jwtTokens.vectors) {
    if ('expectError' in v && v.expectError) continue;

    it(v.name, () => {
      const ttlSeconds = v.exp - v.iat;
      const cfg = {
        UDP_RELAY_BASE_URL: 'https://relay.example.test',
        UDP_RELAY_AUTH_MODE: 'jwt',
        UDP_RELAY_API_KEY: '',
        UDP_RELAY_JWT_SECRET: v.secret,
        UDP_RELAY_TOKEN_TTL_SECONDS: ttlSeconds,
        UDP_RELAY_AUDIENCE: v.aud ?? '',
        UDP_RELAY_ISSUER: v.iss ?? '',
      } as unknown as Config;

      const tokenInfo = mintUdpRelayToken(cfg, {
        sessionId: v.sid,
        origin: v.origin,
        nowMs: v.nowSec * 1000,
      });
      assert.ok(tokenInfo, 'expected mintUdpRelayToken to return token');
      assert.equal(tokenInfo.authMode, 'jwt');
      assert.equal(tokenInfo.token, v.token);
      assert.equal(tokenInfo.expiresAt, new Date(v.exp * 1000).toISOString());
    });
  }
});

