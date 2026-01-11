import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { describe, it } from 'node:test';
import { fileURLToPath } from 'node:url';

import { mintSessionToken, verifySessionToken } from '../src/session.js';

type VectorsFile = {
  schema: number;
  sessionTokens: {
    vectors: Array<
      | {
          name: string;
          secret: string;
          token: string;
          sid: string;
          exp: number;
          nowMs: number;
        }
      | {
          name: string;
          secret: string;
          token: string;
          nowMs: number;
          expectError: true;
        }
    >;
  };
};

function vectorsPath(): string {
  const dir = path.dirname(fileURLToPath(import.meta.url));
  return path.resolve(dir, '..', '..', '..', 'protocol-vectors', 'auth-tokens.json');
}

describe('gateway session token vectors', () => {
  const vectors = JSON.parse(fs.readFileSync(vectorsPath(), 'utf8')) as VectorsFile;
  assert.equal(vectors.schema, 1);

  for (const v of vectors.sessionTokens.vectors) {
    it(v.name, () => {
      const secret = Buffer.from(v.secret, 'utf8');

      const verified = verifySessionToken(v.token, secret, v.nowMs);
      if ('expectError' in v && v.expectError) {
        assert.equal(verified, null);
        return;
      }

      assert.ok(verified, 'expected token to verify');
      assert.equal(verified.id, v.sid);
      assert.equal(verified.expiresAtMs, v.exp * 1000);

      // Also assert the minting logic is canonical.
      assert.equal(mintSessionToken({ v: 1, sid: v.sid, exp: v.exp }, secret), v.token);
    });
  }
});

