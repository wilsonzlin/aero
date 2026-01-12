import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import type { IncomingHttpHeaders } from 'node:http';
import https from 'node:https';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { buildServer } from '../src/server.js';

function hasOpenssl(): boolean {
  const res = spawnSync('openssl', ['version'], { stdio: 'ignore' });
  return res.status === 0;
}

async function httpsRequest(
  hostname: string,
  port: number,
  pathName: string,
): Promise<{ statusCode: number; headers: IncomingHttpHeaders; body: string }> {
  return await new Promise((resolve, reject) => {
    const req = https.request(
      { hostname, port, path: pathName, method: 'GET', rejectUnauthorized: false },
      (res) => {
        let body = '';
        res.setEncoding('utf8');
        res.on('data', (chunk) => {
          body += chunk;
        });
        res.on('end', () => {
          resolve({
            statusCode: res.statusCode ?? 0,
            headers: res.headers,
            body,
          });
        });
      },
    );
    req.on('error', reject);
    req.end();
  });
}

test('HTTPS server starts and serves /healthz', async (t) => {
  if (!hasOpenssl()) {
    t.skip('openssl not available');
    return;
  }

  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aero-gateway-tls-'));
  const certPath = path.join(dir, 'localhost.crt');
  const keyPath = path.join(dir, 'localhost.key');
  const opensslConfigPath = path.join(dir, 'openssl.cnf');

  fs.writeFileSync(
    opensslConfigPath,
    [
      '[req]',
      'distinguished_name = req_distinguished_name',
      'x509_extensions = v3_req',
      'prompt = no',
      '',
      '[req_distinguished_name]',
      'CN = localhost',
      '',
      '[v3_req]',
      'subjectAltName = @alt_names',
      '',
      '[alt_names]',
      'DNS.1 = localhost',
      'IP.1 = 127.0.0.1',
      '',
    ].join('\n'),
  );

  const res = spawnSync(
    'openssl',
    ['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-sha256', '-days', '1', '-keyout', keyPath, '-out', certPath, '-config', opensslConfigPath],
    { stdio: 'ignore' },
  );
  assert.equal(res.status, 0, 'openssl failed to generate a self-signed certificate');

  const { app } = buildServer({
    HOST: '127.0.0.1',
    PORT: 0,
    LOG_LEVEL: 'silent',
    ALLOWED_ORIGINS: ['http://localhost'],
    PUBLIC_BASE_URL: 'https://localhost',
    SHUTDOWN_GRACE_MS: 100,
    CROSS_ORIGIN_ISOLATION: false,
    RATE_LIMIT_REQUESTS_PER_MINUTE: 0,
    TRUST_PROXY: false,
    SESSION_SECRET: 'test-secret',
    SESSION_TTL_SECONDS: 60 * 60 * 24,
    SESSION_COOKIE_SAMESITE: 'Lax',
    TLS_ENABLED: true,
    TLS_CERT_PATH: certPath,
    TLS_KEY_PATH: keyPath,
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
    DNS_UPSTREAMS: [],
    DNS_UPSTREAM_TIMEOUT_MS: 200,
    DNS_CACHE_MAX_ENTRIES: 0,
    DNS_CACHE_MAX_TTL_SECONDS: 0,
    DNS_CACHE_NEGATIVE_TTL_SECONDS: 0,
    DNS_MAX_QUERY_BYTES: 4096,
    DNS_MAX_RESPONSE_BYTES: 4096,
    DNS_ALLOW_ANY: false,
    DNS_ALLOW_PRIVATE_PTR: false,
    DNS_QPS_PER_IP: 0,
    DNS_BURST_PER_IP: 0,

    UDP_RELAY_BASE_URL: '',
    UDP_RELAY_AUTH_MODE: 'none',
    UDP_RELAY_API_KEY: '',
    UDP_RELAY_JWT_SECRET: '',
    UDP_RELAY_TOKEN_TTL_SECONDS: 300,
    UDP_RELAY_AUDIENCE: '',
    UDP_RELAY_ISSUER: '',
  });

  await app.listen({ host: '127.0.0.1', port: 0 });
  t.after(async () => {
    await app.close();
  });

  const address = app.server.address();
  assert.ok(address && typeof address === 'object', 'server did not bind to a TCP port');

  const healthz = await httpsRequest('localhost', address.port, '/healthz');
  assert.equal(healthz.statusCode, 200);
  assert.deepEqual(JSON.parse(healthz.body), { ok: true });

  const session = await httpsRequest('localhost', address.port, '/session');
  assert.equal(session.statusCode, 200);
  const setCookie = session.headers['set-cookie'];
  assert.ok(setCookie, 'expected Set-Cookie header');
  const cookieHeader = Array.isArray(setCookie) ? setCookie.join('; ') : setCookie;
  assert.match(cookieHeader, /\bSecure\b/, 'expected Secure cookie attribute when served over HTTPS');
});
