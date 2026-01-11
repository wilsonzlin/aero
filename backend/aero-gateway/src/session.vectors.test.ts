import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import { mintSessionToken, verifySessionToken } from './session.js';

type VectorsFile = {
  version: number;
  aero_session: {
    secret: string;
    nowMs: number;
    tokens: {
      valid: { token: string; claims: { sid: string; exp: number } };
      expired: { token: string; claims: { sid: string; exp: number } };
      badSignature: { token: string; claims: { sid: string; exp: number } };
    };
  };
};

function loadVectors(): VectorsFile {
  const here = path.dirname(fileURLToPath(import.meta.url));
  const vectorsPath = path.join(here, '../../../crates/conformance/test-vectors/aero-vectors-v1.json');
  return JSON.parse(readFileSync(vectorsPath, 'utf8')) as VectorsFile;
}

test('aero_session tokens match canonical vectors', () => {
  const vectors = loadVectors();
  assert.equal(vectors.version, 1, 'unexpected vector file version');

  const secret = Buffer.from(vectors.aero_session.secret, 'utf8');
  const nowMs = vectors.aero_session.nowMs;

  assert.equal(
    mintSessionToken({ v: 1, sid: vectors.aero_session.tokens.valid.claims.sid, exp: vectors.aero_session.tokens.valid.claims.exp }, secret),
    vectors.aero_session.tokens.valid.token,
    'mint valid token',
  );
  assert.equal(
    mintSessionToken({ v: 1, sid: vectors.aero_session.tokens.expired.claims.sid, exp: vectors.aero_session.tokens.expired.claims.exp }, secret),
    vectors.aero_session.tokens.expired.token,
    'mint expired token',
  );

  const verified = verifySessionToken(vectors.aero_session.tokens.valid.token, secret, nowMs);
  assert.ok(verified, 'expected valid token to verify');
  assert.equal(verified.id, vectors.aero_session.tokens.valid.claims.sid);
  assert.equal(verified.expiresAtMs, vectors.aero_session.tokens.valid.claims.exp * 1000);

  assert.equal(
    verifySessionToken(vectors.aero_session.tokens.expired.token, secret, nowMs),
    null,
    'expected expired token to be rejected',
  );
  assert.equal(
    verifySessionToken(vectors.aero_session.tokens.badSignature.token, secret, nowMs),
    null,
    'expected bad signature token to be rejected',
  );
});

