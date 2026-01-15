import assert from 'node:assert/strict';
import test from 'node:test';

import { base64UrlPrefixForHeader, decodeBase64UrlToBuffer, encodeBase64Url, maxBase64UrlLenForBytes } from '../src/base64url.js';

test('RFC8484 GET base64url decoding (unpadded)', () => {
  const original = Buffer.from([0, 1, 2, 3, 4, 250, 251, 252, 253, 254, 255]);
  const encoded = encodeBase64Url(original);
  const decoded = decodeBase64UrlToBuffer(encoded);
  assert.deepEqual(decoded, original);
});

test('base64url decoding rejects invalid length', () => {
  assert.throws(() => decodeBase64UrlToBuffer('a'), /Invalid base64url length/);
});

test('base64url canonical mode rejects non-canonical unused bits (len%4==2)', () => {
  const canonical = 'AA'; // 1 byte of 0x00
  const nonCanonical = 'AB'; // decodes to same byte, but has unused low 4 bits set

  assert.deepEqual(decodeBase64UrlToBuffer(canonical, { canonical: true }), Buffer.from([0]));
  assert.deepEqual(decodeBase64UrlToBuffer(nonCanonical), Buffer.from([0]));
  assert.throws(() => decodeBase64UrlToBuffer(nonCanonical, { canonical: true }), /Invalid base64url/);
});

test('base64url canonical mode rejects non-canonical unused bits (len%4==3)', () => {
  const canonical = 'AAA'; // 2 bytes of 0x00
  const nonCanonical = 'AAB'; // decodes to same bytes, but has unused low 2 bits set

  assert.deepEqual(decodeBase64UrlToBuffer(canonical, { canonical: true }), Buffer.from([0, 0]));
  assert.deepEqual(decodeBase64UrlToBuffer(nonCanonical), Buffer.from([0, 0]));
  assert.throws(() => decodeBase64UrlToBuffer(nonCanonical, { canonical: true }), /Invalid base64url/);
});

test('maxBase64UrlLenForBytes matches Node base64url output length', () => {
  for (const n of [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 31, 32, 33, 255, 256, 257]) {
    const buf = Buffer.alloc(n, 0xab);
    assert.equal(buf.toString('base64url').length, maxBase64UrlLenForBytes(n));
  }

  assert.equal(maxBase64UrlLenForBytes(-1), 0);
  assert.equal(maxBase64UrlLenForBytes(Number.NaN), 0);
  assert.equal(maxBase64UrlLenForBytes(Number.POSITIVE_INFINITY), 0);
  assert.equal(maxBase64UrlLenForBytes(Number.NEGATIVE_INFINITY), 0);

  // Saturation for absurd sizes: should remain a safe integer.
  assert.equal(maxBase64UrlLenForBytes(Number.MAX_SAFE_INTEGER), Number.MAX_SAFE_INTEGER);
});

test('base64UrlPrefixForHeader never returns len%4==1', () => {
  const raw = 'a'.repeat(128);
  for (let maxChars = 0; maxChars <= 32; maxChars += 1) {
    const prefix = base64UrlPrefixForHeader(raw, maxChars);
    assert.ok(prefix.length <= maxChars);
    assert.ok(prefix.length <= raw.length);
    assert.notEqual(prefix.length % 4, 1);
  }

  assert.equal(base64UrlPrefixForHeader('abcd', 1), '');
});
