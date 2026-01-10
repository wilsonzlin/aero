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
