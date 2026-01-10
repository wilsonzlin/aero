import assert from 'node:assert/strict';
import test from 'node:test';

import { dnsResponseToJson } from '../src/dns/dnsJson.js';
import { DNS_RECORD_TYPES, parseDnsRecordType } from '../src/dns/recordTypes.js';
import { encodeDnsName } from '../src/dns/codec.js';

function buildResponseWithAAnswer(options: {
  name: string;
  ttl: number;
  ipv4: [number, number, number, number];
}): Buffer {
  const qname = encodeDnsName(options.name);
  const question = Buffer.concat([qname, Buffer.from([0x00, 0x01, 0x00, 0x01])]);

  const answer = Buffer.from([
    0xc0,
    0x0c,
    0x00,
    0x01,
    0x00,
    0x01,
    (options.ttl >>> 24) & 0xff,
    (options.ttl >>> 16) & 0xff,
    (options.ttl >>> 8) & 0xff,
    options.ttl & 0xff,
    0x00,
    0x04,
    ...options.ipv4,
  ]);

  const header = Buffer.alloc(12);
  header.writeUInt16BE(0, 0);
  header.writeUInt16BE(0x8180, 2);
  header.writeUInt16BE(1, 4);
  header.writeUInt16BE(1, 6);
  header.writeUInt16BE(0, 8);
  header.writeUInt16BE(0, 10);

  return Buffer.concat([header, question, answer]);
}

function buildResponseWithAaaaAnswer(options: { name: string; ttl: number; ipv6: number[] }): Buffer {
  const qname = encodeDnsName(options.name);
  const question = Buffer.concat([qname, Buffer.from([0x00, 0x1c, 0x00, 0x01])]);

  const answer = Buffer.from([
    0xc0,
    0x0c,
    0x00,
    0x1c,
    0x00,
    0x01,
    (options.ttl >>> 24) & 0xff,
    (options.ttl >>> 16) & 0xff,
    (options.ttl >>> 8) & 0xff,
    options.ttl & 0xff,
    0x00,
    0x10,
    ...options.ipv6,
  ]);

  const header = Buffer.alloc(12);
  header.writeUInt16BE(0, 0);
  header.writeUInt16BE(0x8180, 2);
  header.writeUInt16BE(1, 4);
  header.writeUInt16BE(1, 6);
  header.writeUInt16BE(0, 8);
  header.writeUInt16BE(0, 10);

  return Buffer.concat([header, question, answer]);
}

function buildResponseWithCnameAnswer(options: { name: string; ttl: number; target: string }): Buffer {
  const qname = encodeDnsName(options.name);
  const question = Buffer.concat([qname, Buffer.from([0x00, 0x01, 0x00, 0x01])]);
  const targetName = encodeDnsName(options.target);

  const answer = Buffer.from([
    0xc0,
    0x0c,
    0x00,
    0x05,
    0x00,
    0x01,
    (options.ttl >>> 24) & 0xff,
    (options.ttl >>> 16) & 0xff,
    (options.ttl >>> 8) & 0xff,
    options.ttl & 0xff,
    (targetName.length >>> 8) & 0xff,
    targetName.length & 0xff,
    ...targetName,
  ]);

  const header = Buffer.alloc(12);
  header.writeUInt16BE(0, 0);
  header.writeUInt16BE(0x8180, 2);
  header.writeUInt16BE(1, 4);
  header.writeUInt16BE(1, 6);
  header.writeUInt16BE(0, 8);
  header.writeUInt16BE(0, 10);

  return Buffer.concat([header, question, answer]);
}

test('parseDnsRecordType supports names and numeric values for supported types', () => {
  assert.equal(parseDnsRecordType('A'), DNS_RECORD_TYPES.A);
  assert.equal(parseDnsRecordType('1'), DNS_RECORD_TYPES.A);
  assert.equal(parseDnsRecordType('AAAA'), DNS_RECORD_TYPES.AAAA);
  assert.equal(parseDnsRecordType('28'), DNS_RECORD_TYPES.AAAA);
  assert.equal(parseDnsRecordType('CNAME'), DNS_RECORD_TYPES.CNAME);
  assert.throws(() => parseDnsRecordType('TXT'));
  assert.throws(() => parseDnsRecordType('16'));
});

test('dnsResponseToJson formats Cloudflare-style fields', () => {
  const response = buildResponseWithAAnswer({
    name: 'example.com',
    ttl: 300,
    ipv4: [93, 184, 216, 34],
  });

  const json = dnsResponseToJson(response, {
    name: 'example.com',
    type: DNS_RECORD_TYPES.A,
  });

  assert.deepEqual(json, {
    Status: 0,
    TC: false,
    RD: true,
    RA: true,
    AD: false,
    CD: false,
    Question: [{ name: 'example.com', type: 1 }],
    Answer: [{ name: 'example.com', type: 1, TTL: 300, data: '93.184.216.34' }],
  });
});

test('dnsResponseToJson formats AAAA and CNAME answers', () => {
  const ipv6 = [
    0x20,
    0x01,
    0x0d,
    0xb8,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    1,
  ];

  const aaaaResponse = buildResponseWithAaaaAnswer({ name: 'example.com', ttl: 120, ipv6 });
  const aaaaJson = dnsResponseToJson(aaaaResponse, { name: 'example.com', type: DNS_RECORD_TYPES.AAAA });
  assert.equal(aaaaJson.Answer?.[0]?.data, '2001:db8::1');

  const cnameResponse = buildResponseWithCnameAnswer({ name: 'example.com', ttl: 180, target: 'alias.example.net' });
  const cnameJson = dnsResponseToJson(cnameResponse, { name: 'example.com', type: DNS_RECORD_TYPES.A });
  assert.equal(cnameJson.Answer?.[0]?.type, DNS_RECORD_TYPES.CNAME);
  assert.equal(cnameJson.Answer?.[0]?.data, 'alias.example.net');
});

test('dnsResponseToJson omits Answer when empty', () => {
  const qname = encodeDnsName('example.com');
  const question = Buffer.concat([qname, Buffer.from([0x00, 0x01, 0x00, 0x01])]);

  const header = Buffer.alloc(12);
  header.writeUInt16BE(0, 0);
  header.writeUInt16BE(0x8180, 2);
  header.writeUInt16BE(1, 4);
  header.writeUInt16BE(0, 6);
  header.writeUInt16BE(0, 8);
  header.writeUInt16BE(0, 10);

  const response = Buffer.concat([header, question]);
  const json = dnsResponseToJson(response, {
    name: 'example.com',
    type: DNS_RECORD_TYPES.A,
  });

  assert.equal(json.Answer, undefined);
  assert.equal(json.Status, 0);
  assert.deepEqual(json.Question, [{ name: 'example.com', type: 1 }]);
});

