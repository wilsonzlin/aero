import assert from 'node:assert/strict';
import test from 'node:test';

import { encodeDnsName, readDnsName } from '../src/dns/codec.js';

test('readDnsName rejects DNS names longer than 255 bytes (RFC1035)', () => {
  // 4 labels * (1 length byte + 63 label bytes) + 1 terminator = 257 bytes (>255).
  const labels = ['a', 'b', 'c', 'd'].map((ch) => Buffer.alloc(63, ch));
  const encoded: Buffer[] = [];
  for (const label of labels) {
    encoded.push(Buffer.from([label.length]), label);
  }
  encoded.push(Buffer.from([0x00]));
  const message = Buffer.concat(encoded);

  assert.throws(() => readDnsName(message, 0), /DNS name too long/);
});

test('encodeDnsName rejects DNS names longer than 255 bytes (RFC1035)', () => {
  const labels = ['a', 'b', 'c', 'd'].map((ch) => ch.repeat(63));
  const name = labels.join('.');
  assert.throws(() => encodeDnsName(name), /DNS name too long/);
});
