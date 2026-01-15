import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import type { IncomingHttpHeaders } from 'node:http';
import https from 'node:https';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { buildServer } from '../src/server.js';
import { makeTestConfig } from './testConfig.js';

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

  const { app } = buildServer(
    makeTestConfig({
      PUBLIC_BASE_URL: 'https://localhost',
      TLS_ENABLED: true,
      TLS_CERT_PATH: certPath,
      TLS_KEY_PATH: keyPath,
      TCP_ALLOW_PRIVATE_IPS: false,
      TCP_PROXY_MAX_CONNECTIONS: 64,
      DNS_ALLOW_ANY: false,
      DNS_ALLOW_PRIVATE_PTR: false,
    }),
  );

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
