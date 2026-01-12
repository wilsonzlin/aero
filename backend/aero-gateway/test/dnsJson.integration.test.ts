import assert from 'node:assert/strict';
import * as dgram from 'node:dgram';
import test from 'node:test';

import { buildServer } from '../src/server.js';
import { decodeDnsHeader, decodeFirstQuestion, encodeDnsQuery, encodeDnsResponseA } from '../src/dns/codec.js';

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

  UDP_RELAY_BASE_URL: '',
  UDP_RELAY_AUTH_MODE: 'none' as const,
  UDP_RELAY_API_KEY: '',
  UDP_RELAY_JWT_SECRET: '',
  UDP_RELAY_TOKEN_TTL_SECONDS: 300,
  UDP_RELAY_AUDIENCE: '',
  UDP_RELAY_ISSUER: '',
};

async function createSessionCookie(app: import('fastify').FastifyInstance): Promise<string> {
  const res = await app.inject({ method: 'POST', url: '/session' });
  if (res.statusCode !== 201) throw new Error(`Failed to create session: ${res.statusCode}`);
  const setCookie = res.headers['set-cookie'];
  const raw = Array.isArray(setCookie) ? setCookie[0] : setCookie;
  if (!raw) throw new Error('Missing Set-Cookie header');
  return raw.split(';')[0] ?? raw;
}

test('GET /dns-json resolves via UDP upstream and shares cache with /dns-query', async () => {
  const upstream = dgram.createSocket('udp4');
  let queryCount = 0;

  upstream.on('message', (msg, rinfo) => {
    queryCount += 1;
    const header = decodeDnsHeader(msg);
    const question = decodeFirstQuestion(msg);
    const response = encodeDnsResponseA({
      id: header.id,
      question,
      answers: [{ name: question.name, ttl: 60, address: '203.0.113.1' }],
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
    const r1 = await app.inject({
      method: 'GET',
      url: '/dns-json?name=example.com&type=A',
      headers: { cookie },
    });
    assert.equal(r1.statusCode, 200);
    assert.ok(String(r1.headers['content-type'] ?? '').startsWith('application/dns-json'));
    const json = JSON.parse(r1.body);
    assert.equal(json.Status, 0);
    assert.equal(json.Answer?.[0]?.data, '203.0.113.1');
    assert.equal(queryCount, 1);

    const q2 = encodeDnsQuery({ id: 0x1234, name: 'example.com', type: 1 });
    const r2 = await app.inject({
      method: 'POST',
      url: '/dns-query',
      headers: { 'content-type': 'application/dns-message', cookie },
      payload: q2,
    });
    assert.equal(r2.statusCode, 200);
    assert.ok(String(r2.headers['content-type'] ?? '').startsWith('application/dns-message'));
    const b2 = r2.rawPayload;

    const header2 = decodeDnsHeader(b2);
    assert.equal(header2.id, 0x1234);
    assert.equal(queryCount, 1, 'second query should be served from shared cache');
  } finally {
    upstream.close();
    await app.close();
  }
});
