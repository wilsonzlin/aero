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
  TLS_ENABLED: false,
  TLS_CERT_PATH: '',
  TLS_KEY_PATH: '',

  SESSION_SECRET: 'test-secret',
  SESSION_TTL_SECONDS: 60 * 60 * 24,
  SESSION_COOKIE_SAMESITE: 'Lax' as const,

  RATE_LIMIT_REQUESTS_PER_MINUTE: 0,

  TCP_ALLOW_PRIVATE_IPS: false,
  TCP_ALLOWED_HOSTS: [],
  TCP_ALLOWED_PORTS: [],
  TCP_BLOCKED_CLIENT_IPS: [],
  TCP_MUX_MAX_STREAMS: 1024,
  TCP_MUX_MAX_STREAM_BUFFER_BYTES: 1024 * 1024,
  TCP_MUX_MAX_FRAME_PAYLOAD_BYTES: 16 * 1024 * 1024,

  TCP_PROXY_MAX_CONNECTIONS: 64,
  TCP_PROXY_MAX_CONNECTIONS_PER_IP: 0,
  TCP_PROXY_MAX_MESSAGE_BYTES: 1024 * 1024,
  TCP_PROXY_CONNECT_TIMEOUT_MS: 10_000,
  TCP_PROXY_IDLE_TIMEOUT_MS: 300_000,

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

  UDP_RELAY_BASE_URL: '',
  UDP_RELAY_AUTH_MODE: 'none' as const,
  UDP_RELAY_API_KEY: '',
  UDP_RELAY_JWT_SECRET: '',
  UDP_RELAY_TOKEN_TTL_SECONDS: 300,
  UDP_RELAY_AUDIENCE: '',
  UDP_RELAY_ISSUER: '',
};

async function listen(app: import('fastify').FastifyInstance): Promise<number> {
  await app.listen({ host: '127.0.0.1', port: 0 });
  const address = app.server.address();
  if (!address || typeof address === 'string') throw new Error('Expected TCP address');
  return address.port;
}

async function createSessionCookie(port: number): Promise<string> {
  const res = await fetch(`http://127.0.0.1:${port}/session`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({}),
  });
  if (!res.ok) throw new Error(`Failed to create session: ${res.status}`);
  const setCookie = res.headers.get('set-cookie');
  if (!setCookie) throw new Error('Missing Set-Cookie header');
  return setCookie.split(';')[0] ?? setCookie;
}

test('ANY query is blocked by default', async () => {
  const { app } = buildServer({ ...baseConfig, DNS_ALLOW_ANY: false });
  await app.ready();
  const port = await listen(app);
  const cookie = await createSessionCookie(port);

  try {
    const query = encodeDnsQuery({ id: 1, name: 'example.com', type: 255 });
    const res = await fetch(`http://127.0.0.1:${port}/dns-query`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/dns-message', cookie },
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
  const cookie = await createSessionCookie(port);

  try {
    const query = encodeDnsQuery({ id: 1, name: 'example.com', type: 1 });
    const res = await fetch(`http://127.0.0.1:${port}/dns-query`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/dns-message', cookie },
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
