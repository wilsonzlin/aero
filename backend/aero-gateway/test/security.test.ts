import assert from 'node:assert/strict';
import * as dgram from 'node:dgram';
import test from 'node:test';

import { buildServer } from '../src/server.js';
import { decodeDnsHeader, encodeDnsQuery, getRcodeFromFlags } from '../src/dns/codec.js';
import { makeTestConfig } from './testConfig.js';

const baseConfig = makeTestConfig({
  TCP_ALLOW_PRIVATE_IPS: false,
  TCP_PROXY_MAX_CONNECTIONS: 64,

  DNS_UPSTREAMS: ['127.0.0.1:53'],
  DNS_UPSTREAM_TIMEOUT_MS: 200,
  DNS_CACHE_MAX_ENTRIES: 1000,
  DNS_CACHE_MAX_TTL_SECONDS: 300,
  DNS_CACHE_NEGATIVE_TTL_SECONDS: 60,
  DNS_MAX_QUERY_BYTES: 4096,
  DNS_MAX_RESPONSE_BYTES: 4096,
  DNS_ALLOW_ANY: false,
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

test('ANY query is blocked by default', async () => {
  const { app } = buildServer({ ...baseConfig, DNS_ALLOW_ANY: false });
  await app.ready();
  const cookie = await createSessionCookie(app);

  try {
    const query = encodeDnsQuery({ id: 1, name: 'example.com', type: 255 });
    const res = await app.inject({
      method: 'POST',
      url: '/dns-query',
      headers: { 'content-type': 'application/dns-message', cookie },
      payload: query,
    });

    assert.equal(res.statusCode, 200);
    assert.ok(String(res.headers['content-type'] ?? '').startsWith('application/dns-message'));
    const body = res.rawPayload;
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
  const cookie = await createSessionCookie(app);

  try {
    const query = encodeDnsQuery({ id: 1, name: 'example.com', type: 1 });
    const res = await app.inject({
      method: 'POST',
      url: '/dns-query',
      headers: { 'content-type': 'application/dns-message', cookie },
      payload: query,
    });

    assert.equal(res.statusCode, 200);
    assert.ok(String(res.headers['content-type'] ?? '').startsWith('application/dns-message'));
    const body = res.rawPayload;
    const header = decodeDnsHeader(body);
    assert.equal(header.id, 1);
    assert.equal(getRcodeFromFlags(header.flags), 2);
  } finally {
    upstream.close();
    await app.close();
  }
});
