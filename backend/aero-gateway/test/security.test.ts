import assert from 'node:assert/strict';
import * as dgram from 'node:dgram';
import test from 'node:test';

import { buildServer } from '../src/server.js';
import { decodeDnsHeader, encodeDnsQuery, getRcodeFromFlags } from '../src/dns/codec.js';

function bufferToArrayBuffer(buf: Buffer): ArrayBuffer {
  // Node buffers are backed by ArrayBuffer at runtime, but the type is
  // `ArrayBufferLike`. Cast to satisfy `fetch`/undici typings.
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

  TLS_ENABLED: false,
  TLS_CERT_PATH: '',
  TLS_KEY_PATH: '',

  TCP_PROXY_MAX_CONNECTIONS: 0,
  TCP_PROXY_MAX_CONNECTIONS_PER_IP: 0,

  DNS_UPSTREAMS: ['127.0.0.1:53'],
  DNS_UPSTREAM_TIMEOUT_MS: 200,
  DNS_CACHE_MAX_ENTRIES: 1000,
  DNS_CACHE_MAX_TTL_SECONDS: 300,
  DNS_CACHE_NEGATIVE_TTL_SECONDS: 60,
  DNS_MAX_QUERY_BYTES: 4096,
  DNS_MAX_RESPONSE_BYTES: 4096,
  DNS_ALLOW_ANY: false,
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

test('ANY query is blocked by default', async () => {
  const { app } = buildServer({ ...baseConfig, DNS_ALLOW_ANY: false });
  await app.ready();
  const port = await listen(app);

  try {
    const query = encodeDnsQuery({ id: 1, name: 'example.com', type: 255 });
    const res = await fetch(`http://127.0.0.1:${port}/dns-query`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/dns-message' },
      body: bufferToArrayBuffer(query),
    });

    assert.equal(res.status, 200);
    assert.ok((res.headers.get('content-type') ?? '').startsWith('application/dns-message'));
    const body = Buffer.from(await res.arrayBuffer());
    const header = decodeDnsHeader(body);
    assert.equal(header.id, 1);
    assert.equal(getRcodeFromFlags(header.flags), 5);
  } finally {
    await app.close();
  }
});

test('response size cap rejects large upstream responses', async () => {
  const upstream = dgram.createSocket('udp4');

  upstream.on('message', (msg, rinfo) => {
    const padded = Buffer.concat([msg, Buffer.alloc(200)]);
    upstream.send(padded, rinfo.port, rinfo.address);
  });

  await new Promise<void>((resolve) => upstream.bind(0, '127.0.0.1', resolve));
  const upstreamAddr = upstream.address();
  if (!upstreamAddr || typeof upstreamAddr === 'string') throw new Error('Expected UDP address');

  const { app } = buildServer({
    ...baseConfig,
    DNS_UPSTREAMS: [`127.0.0.1:${upstreamAddr.port}`],
    DNS_MAX_RESPONSE_BYTES: 50,
  });
  await app.ready();
  const port = await listen(app);

  try {
    const query = encodeDnsQuery({ id: 1, name: 'example.com', type: 1 });
    const res = await fetch(`http://127.0.0.1:${port}/dns-query`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/dns-message' },
      body: bufferToArrayBuffer(query),
    });

    assert.equal(res.status, 200);
    assert.ok((res.headers.get('content-type') ?? '').startsWith('application/dns-message'));
    const body = Buffer.from(await res.arrayBuffer());
    const header = decodeDnsHeader(body);
    assert.equal(header.id, 1);
    assert.equal(getRcodeFromFlags(header.flags), 2);
  } finally {
    upstream.close();
    await app.close();
  }
});
