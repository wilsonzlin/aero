import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { describe, it } from 'node:test';
import { fileURLToPath } from 'node:url';

import { mintSessionToken, verifySessionToken } from '../src/session.js';

type VectorsFile = {
  schema: number;
  sessionTokens: {
    testSecret: string;
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

type ConformanceVectorsFile = {
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

function vectorsPath(): string {
  const dir = path.dirname(fileURLToPath(import.meta.url));
  return path.resolve(dir, '..', '..', '..', 'protocol-vectors', 'auth-tokens.json');
}

function conformanceVectorsPath(): string {
  const dir = path.dirname(fileURLToPath(import.meta.url));
  return path.resolve(dir, '..', '..', '..', 'crates', 'conformance', 'test-vectors', 'aero-vectors-v1.json');
}

describe('gateway session token vectors', () => {
  const vectors = JSON.parse(fs.readFileSync(vectorsPath(), 'utf8')) as VectorsFile;
  assert.equal(vectors.schema, 1);

  const conformance = JSON.parse(fs.readFileSync(conformanceVectorsPath(), 'utf8')) as ConformanceVectorsFile;
  assert.equal(conformance.version, 1, 'unexpected conformance vector file version');

  it('matches unified conformance vectors', () => {
    assert.equal(conformance.aero_session.secret, vectors.sessionTokens.testSecret);

    const byName = new Map(vectors.sessionTokens.vectors.map((v) => [v.name, v] as const));

    const vValid = byName.get('valid');
    assert.ok(vValid && !('expectError' in vValid), 'missing valid token vector');
    assert.equal(vValid.token, conformance.aero_session.tokens.valid.token);
    assert.equal(vValid.sid, conformance.aero_session.tokens.valid.claims.sid);
    assert.equal(vValid.exp, conformance.aero_session.tokens.valid.claims.exp);
    assert.equal(vValid.nowMs, conformance.aero_session.nowMs);

    const vExpired = byName.get('expired');
    assert.ok(vExpired, 'missing expired token vector');
    assert.equal(vExpired.token, conformance.aero_session.tokens.expired.token);

    const vBadSig = byName.get('badSignature');
    assert.ok(vBadSig, 'missing badSignature token vector');
    assert.equal(vBadSig.token, conformance.aero_session.tokens.badSignature.token);
  });

  for (const v of vectors.sessionTokens.vectors) {
    it(v.name, () => {
      const secret = Buffer.from(v.secret, 'utf8');

      const verified = verifySessionToken(v.token, secret, v.nowMs);
      if ('expectError' in v) {
        assert.equal(verified, null);
        return;
      }

      assert.ok(verified, 'expected token to verify');
      assert.equal(verified.id, v.sid);
      assert.equal(verified.expiresAtMs, v.exp * 1000);

      // Also assert the minting logic is canonical.
      if (v.name === 'valid') {
        assert.equal(mintSessionToken({ v: 1, sid: v.sid, exp: v.exp }, secret), v.token);
      }
    });
  }

  it('rejects oversized tokens early (DoS guard)', () => {
    const nowMs = conformance.aero_session.nowMs;
    const secret = Buffer.from(conformance.aero_session.secret, 'utf8');
    const token = 'A'.repeat(20_000);
    assert.equal(verifySessionToken(token, secret, nowMs), null);
  });

  it('rejects non-43-char signature segments', () => {
    const nowMs = conformance.aero_session.nowMs;
    const secret = Buffer.from(conformance.aero_session.secret, 'utf8');
    const payloadB64 = Buffer.from(JSON.stringify({ v: 1, sid: 'sid', exp: Math.floor(nowMs / 1000) + 60 }), 'utf8').toString(
      'base64url',
    );
    const token = `${payloadB64}.${'A'.repeat(42)}`;
    assert.equal(verifySessionToken(token, secret, nowMs), null);
  });

  it('refuses to mint unverifiable oversized payloads', () => {
    const secret = Buffer.from(conformance.aero_session.secret, 'utf8');
    const sid = 's'.repeat(50_000);
    assert.throws(
      () => mintSessionToken({ v: 1, sid, exp: 1_700_000_000 }, secret),
      /Session token payload too long/,
    );
  });
});
