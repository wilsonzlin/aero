import assert from 'node:assert/strict';
import * as dgram from 'node:dgram';
import test from 'node:test';

import { buildServer } from '../src/server.js';
import { decodeDnsHeader, decodeFirstQuestion, encodeDnsQuery, encodeDnsResponseA } from '../src/dns/codec.js';
import { makeTestConfig } from './testConfig.js';

const baseConfig = makeTestConfig({
  TCP_ALLOW_PRIVATE_IPS: false,
  TCP_PROXY_MAX_CONNECTIONS: 64,

  DNS_UPSTREAMS: ['127.0.0.1:53'],
  DNS_UPSTREAM_TIMEOUT_MS: 500,
  DNS_CACHE_MAX_ENTRIES: 1000,
  DNS_CACHE_MAX_TTL_SECONDS: 300,
  DNS_CACHE_NEGATIVE_TTL_SECONDS: 60,
  DNS_MAX_QUERY_BYTES: 4096,
  DNS_MAX_RESPONSE_BYTES: 4096,
  DNS_QPS_PER_IP: 1000,
  DNS_BURST_PER_IP: 1000,
});

async function createSessionCookie(app: import('fastify').FastifyInstance): Promise<string> {
  const res = await app.inject({ method: 'POST', url: '/session' });
  if (res.statusCode !== 201) throw new Error(`Failed to create session: ${res.statusCode}`);
  const setCookie = res.headers['set-cookie'];
  const raw = Array.isArray(setCookie) ? setCookie[0] : setCookie;
  if (!raw) throw new Error('Missing Set-Cookie header');
  return raw.split(';')[0] ?? raw;
}

test('DoH POST resolves via UDP upstream and hits cache on subsequent queries', async () => {
  const upstream = dgram.createSocket('udp4');
  let queryCount = 0;

  upstream.on('message', (msg, rinfo) => {
    queryCount += 1;
    const header = decodeDnsHeader(msg);
    const question = decodeFirstQuestion(msg);
    const response = encodeDnsResponseA({
      id: header.id,
      question,
      answers: [{ name: question.name, ttl: 60, address: '93.184.216.34' }],
    });
    upstream.send(response, rinfo.port, rinfo.address);
  });

  await new Promise<void>((resolve) => upstream.bind(0, '127.0.0.1', resolve));
  const upstreamAddr = upstream.address();
  if (!upstreamAddr || typeof upstreamAddr === 'string') throw new Error('Expected UDP address');

  const { app } = buildServer({
    ...baseConfig,
    DNS_UPSTREAMS: [`127.0.0.1:${upstreamAddr.port}`],
  });
  await app.ready();
  const cookie = await createSessionCookie(app);

  try {
    const q1 = encodeDnsQuery({ id: 100, name: 'example.com', type: 1 });
    const r1 = await app.inject({
      method: 'POST',
      url: '/dns-query',
      headers: { 'content-type': 'application/dns-message', cookie },
      payload: q1,
    });
    assert.equal(r1.statusCode, 200);
    assert.ok(String(r1.headers['content-type'] ?? '').startsWith('application/dns-message'));
    const b1 = r1.rawPayload;

    const expected1 = encodeDnsResponseA({
      id: 100,
      question: { name: 'example.com', type: 1, class: 1 },
      answers: [{ name: 'example.com', ttl: 60, address: '93.184.216.34' }],
    });
    assert.deepEqual(b1, expected1);
    assert.equal(queryCount, 1);

    const q2 = encodeDnsQuery({ id: 101, name: 'example.com', type: 1 });
    const r2 = await app.inject({
      method: 'POST',
      url: '/dns-query',
      headers: { 'content-type': 'application/dns-message', cookie },
      payload: q2,
    });
    assert.equal(r2.statusCode, 200);
    const b2 = r2.rawPayload;

    const expected2 = encodeDnsResponseA({
      id: 101,
      question: { name: 'example.com', type: 1, class: 1 },
      answers: [{ name: 'example.com', ttl: 60, address: '93.184.216.34' }],
    });
    assert.deepEqual(b2, expected2);
    assert.equal(queryCount, 1, 'second query should be served from cache');
  } finally {
    upstream.close();
    await app.close();
  }
});
