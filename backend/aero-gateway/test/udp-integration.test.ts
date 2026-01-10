import assert from 'node:assert/strict';
import * as dgram from 'node:dgram';
import test from 'node:test';

import { buildServer } from '../src/server.js';
import { decodeDnsHeader, decodeFirstQuestion, encodeDnsQuery, encodeDnsResponseA } from '../src/dns/codec.js';

function bufferToArrayBuffer(buf: Buffer): ArrayBuffer {
  const ab = buf.buffer as ArrayBuffer;
  return ab.slice(buf.byteOffset, buf.byteOffset + buf.byteLength);
}

const baseConfig = {
  HOST: '127.0.0.1',
  PORT: 0,
  LOG_LEVEL: 'silent' as const,
  ALLOWED_ORIGINS: ['http://localhost'],
  PUBLIC_BASE_URL: 'http://localhost',
  SHUTDOWN_GRACE_MS: 100,
  CROSS_ORIGIN_ISOLATION: false,
  TRUST_PROXY: false,

  RATE_LIMIT_REQUESTS_PER_MINUTE: 0,

  TCP_PROXY_MAX_CONNECTIONS: 0,
  TCP_PROXY_MAX_CONNECTIONS_PER_IP: 0,

  DNS_UPSTREAMS: ['127.0.0.1:53'],
  DNS_UPSTREAM_TIMEOUT_MS: 500,
  DNS_CACHE_MAX_ENTRIES: 1000,
  DNS_CACHE_MAX_TTL_SECONDS: 300,
  DNS_CACHE_NEGATIVE_TTL_SECONDS: 60,
  DNS_MAX_QUERY_BYTES: 4096,
  DNS_MAX_RESPONSE_BYTES: 4096,
  DNS_ALLOW_ANY: true,
  DNS_ALLOW_PRIVATE_PTR: true,
  DNS_QPS_PER_IP: 1000,
  DNS_BURST_PER_IP: 1000,
};

async function listen(app: import('fastify').FastifyInstance): Promise<number> {
  await app.listen({ host: '127.0.0.1', port: 0 });
  const address = app.server.address();
  if (!address || typeof address === 'string') throw new Error('Expected TCP address');
  return address.port;
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
  const port = await listen(app);

  try {
    const q1 = encodeDnsQuery({ id: 100, name: 'example.com', type: 1 });
    const r1 = await fetch(`http://127.0.0.1:${port}/dns-query`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/dns-message' },
      body: bufferToArrayBuffer(q1),
    });
    assert.equal(r1.status, 200);
    assert.ok((r1.headers.get('content-type') ?? '').startsWith('application/dns-message'));
    const b1 = Buffer.from(await r1.arrayBuffer());

    const expected1 = encodeDnsResponseA({
      id: 100,
      question: { name: 'example.com', type: 1, class: 1 },
      answers: [{ name: 'example.com', ttl: 60, address: '93.184.216.34' }],
    });
    assert.deepEqual(b1, expected1);
    assert.equal(queryCount, 1);

    const q2 = encodeDnsQuery({ id: 101, name: 'example.com', type: 1 });
    const r2 = await fetch(`http://127.0.0.1:${port}/dns-query`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/dns-message' },
      body: bufferToArrayBuffer(q2),
    });
    assert.equal(r2.status, 200);
    const b2 = Buffer.from(await r2.arrayBuffer());

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
