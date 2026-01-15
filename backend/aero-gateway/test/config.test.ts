import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { loadConfig } from '../src/config.js';
import {
  L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD_BYTES,
  L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD_BYTES,
} from '../src/protocol/l2Tunnel.js';

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
  assert.equal(config.UDP_RELAY_BASE_URL, '');
  assert.equal(config.UDP_RELAY_AUTH_MODE, 'none');
  assert.equal(config.UDP_RELAY_TOKEN_TTL_SECONDS, 300);
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

test('loadConfig: UDP relay api_key auth requires UDP_RELAY_API_KEY when configured', () => {
  assert.throws(
    () => loadConfig({ UDP_RELAY_BASE_URL: 'https://relay.example.com', UDP_RELAY_AUTH_MODE: 'api_key' }),
    /UDP_RELAY_API_KEY is required/i,
  );
  const config = loadConfig({
    UDP_RELAY_BASE_URL: 'https://relay.example.com',
    UDP_RELAY_AUTH_MODE: 'api_key',
    UDP_RELAY_API_KEY: 'dev-key',
  });
  assert.equal(config.UDP_RELAY_AUTH_MODE, 'api_key');
  assert.equal(config.UDP_RELAY_API_KEY, 'dev-key');
});

test('loadConfig: UDP relay jwt auth requires UDP_RELAY_JWT_SECRET when configured', () => {
  assert.throws(
    () => loadConfig({ UDP_RELAY_BASE_URL: 'https://relay.example.com', UDP_RELAY_AUTH_MODE: 'jwt' }),
    /UDP_RELAY_JWT_SECRET is required/i,
  );
  const config = loadConfig({
    UDP_RELAY_BASE_URL: 'https://relay.example.com',
    UDP_RELAY_AUTH_MODE: 'jwt',
    UDP_RELAY_JWT_SECRET: 'secret',
  });
  assert.equal(config.UDP_RELAY_AUTH_MODE, 'jwt');
  assert.equal(config.UDP_RELAY_JWT_SECRET, 'secret');
});

test('loadConfig: UDP relay base URL is validated when set', () => {
  assert.throws(() => loadConfig({ UDP_RELAY_BASE_URL: 'not-a-url' }), /Invalid UDP_RELAY_BASE_URL/i);
});

test('loadConfig accepts ws(s) UDP_RELAY_BASE_URL schemes', () => {
  assert.equal(loadConfig({ UDP_RELAY_BASE_URL: 'ws://relay.example.com' }).UDP_RELAY_BASE_URL, 'ws://relay.example.com');
  assert.equal(
    loadConfig({ UDP_RELAY_BASE_URL: 'wss://relay.example.com' }).UDP_RELAY_BASE_URL,
    'wss://relay.example.com',
  );
});

test('loadConfig surfaces L2 tunnel payload limits from aero-l2-proxy env var aliases', () => {
  const config = loadConfig({
    AERO_L2_MAX_FRAME_PAYLOAD: '9000',
    AERO_L2_MAX_CONTROL_PAYLOAD: '321',
  });
  assert.equal(config.L2_MAX_FRAME_PAYLOAD_BYTES, 9000);
  assert.equal(config.L2_MAX_CONTROL_PAYLOAD_BYTES, 321);
});

test('loadConfig treats zero L2 payload limit env vars as unset (defaults apply)', () => {
  const config = loadConfig({
    AERO_L2_MAX_FRAME_PAYLOAD: '0',
    AERO_L2_MAX_CONTROL_PAYLOAD: '0',
  });
  assert.equal(config.L2_MAX_FRAME_PAYLOAD_BYTES, L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD_BYTES);
  assert.equal(config.L2_MAX_CONTROL_PAYLOAD_BYTES, L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD_BYTES);
});

test('loadConfig rejects invalid TCP_ALLOWED_PORTS entries with a specific error', () => {
  assert.throws(() => loadConfig({ TCP_ALLOWED_PORTS: '443,abc' }), /Invalid TCP_ALLOWED_PORTS number/i);
});

test('loadConfig rejects overly long ALLOWED_ORIGINS lists', () => {
  assert.throws(
    () => loadConfig({ ALLOWED_ORIGINS: 'http://localhost:1,'.repeat(5000) }),
    /Invalid ALLOWED_ORIGINS/i,
  );
});

test('loadConfig rejects overly long DNS_UPSTREAMS lists', () => {
  assert.throws(
    () => loadConfig({ DNS_UPSTREAMS: '1.1.1.1:53,'.repeat(5000) }),
    /Invalid DNS_UPSTREAMS/i,
  );
});
