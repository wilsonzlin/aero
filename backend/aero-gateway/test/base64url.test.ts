import assert from 'node:assert/strict';
import test from 'node:test';

import { decodeBase64UrlToBuffer } from '../src/routes/doh.js';

function encodeBase64Url(buffer: Buffer): string {
  return buffer.toString('base64').replaceAll('=', '').replaceAll('+', '-').replaceAll('/', '_');
}

test('RFC8484 GET base64url decoding (unpadded)', () => {
  const original = Buffer.from([0, 1, 2, 3, 4, 250, 251, 252, 253, 254, 255]);
  const encoded = encodeBase64Url(original);
  const decoded = decodeBase64UrlToBuffer(encoded);
  assert.deepEqual(decoded, original);
});

test('base64url decoding rejects invalid length', () => {
  assert.throws(() => decodeBase64UrlToBuffer('a'), /Invalid base64url length/);
});
