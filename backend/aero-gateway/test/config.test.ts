import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { loadConfig } from '../src/config.js';

test('loadConfig applies defaults and derives ALLOWED_ORIGINS from PUBLIC_BASE_URL', () => {
  const config = loadConfig({});
  assert.equal(config.HOST, '0.0.0.0');
  assert.equal(config.PORT, 8080);
  assert.equal(config.LOG_LEVEL, 'info');
  assert.equal(config.PUBLIC_BASE_URL, 'http://localhost:8080');
  assert.deepEqual(config.ALLOWED_ORIGINS, ['http://localhost:8080']);
  assert.equal(config.TLS_ENABLED, false);
  assert.equal(config.TRUST_PROXY, false);
  assert.equal(config.TCP_ALLOW_PRIVATE_IPS, false);
  assert.equal(config.SESSION_SECRET, '');
  assert.equal(config.SESSION_TTL_SECONDS, 60 * 60 * 24);
  assert.equal(config.SESSION_COOKIE_SAMESITE, 'Lax');
  assert.deepEqual(config.TCP_ALLOWED_HOSTS, []);
  assert.deepEqual(config.TCP_ALLOWED_PORTS, []);
  assert.deepEqual(config.TCP_BLOCKED_CLIENT_IPS, []);
  assert.equal(config.TCP_MUX_MAX_STREAMS, 1024);
  assert.equal(config.TCP_PROXY_MAX_CONNECTIONS, 64);
});

test('loadConfig validates port range', () => {
  assert.throws(() => loadConfig({ PORT: '70000' }), /Invalid configuration/i);
});

test('loadConfig rejects invalid TLS_ENABLED', () => {
  assert.throws(() => loadConfig({ TLS_ENABLED: 'true' }), /Invalid configuration/i);
});

test('loadConfig: TLS enabled requires cert and key paths', () => {
  assert.throws(() => loadConfig({ TLS_ENABLED: '1' }), /TLS_CERT_PATH is required/);
  assert.throws(
    () => loadConfig({ TLS_ENABLED: '1', TLS_CERT_PATH: '/tmp/cert.pem' }),
    /TLS_KEY_PATH is required/,
  );
});

test('loadConfig: TLS enabled validates cert and key files exist', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aero-gateway-'));
  const certPath = path.join(dir, 'cert.pem');
  const keyPath = path.join(dir, 'key.pem');

  fs.writeFileSync(certPath, 'dummy');
  fs.writeFileSync(keyPath, 'dummy');

  const config = loadConfig({
    TLS_ENABLED: '1',
    TLS_CERT_PATH: certPath,
    TLS_KEY_PATH: keyPath,
  });

  assert.equal(config.TLS_ENABLED, true);
  assert.equal(config.TLS_CERT_PATH, certPath);
  assert.equal(config.TLS_KEY_PATH, keyPath);
});

test('loadConfig validates TCP_ALLOWED_PORTS range', () => {
  assert.throws(() => loadConfig({ TCP_ALLOWED_PORTS: '0,443' }), /Invalid TCP_ALLOWED_PORTS entry/i);
  assert.throws(() => loadConfig({ TCP_ALLOWED_PORTS: '65536' }), /Invalid TCP_ALLOWED_PORTS entry/i);
});

test('loadConfig parses TCP_ALLOW_PRIVATE_IPS', () => {
  const config = loadConfig({ TCP_ALLOW_PRIVATE_IPS: '1' });
  assert.equal(config.TCP_ALLOW_PRIVATE_IPS, true);
});
