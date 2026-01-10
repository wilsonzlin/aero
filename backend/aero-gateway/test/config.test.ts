import assert from 'node:assert/strict';
import test from 'node:test';
import { loadConfig } from '../src/config.js';

test('loadConfig applies defaults and derives ALLOWED_ORIGINS from PUBLIC_BASE_URL', () => {
  const config = loadConfig({});
  assert.equal(config.HOST, '0.0.0.0');
  assert.equal(config.PORT, 8080);
  assert.equal(config.LOG_LEVEL, 'info');
  assert.equal(config.PUBLIC_BASE_URL, 'http://localhost:8080');
  assert.deepEqual(config.ALLOWED_ORIGINS, ['http://localhost:8080']);
  assert.equal(config.TRUST_PROXY, false);
});

test('loadConfig validates port range', () => {
  assert.throws(() => loadConfig({ PORT: '70000' }), /Invalid configuration/i);
});
