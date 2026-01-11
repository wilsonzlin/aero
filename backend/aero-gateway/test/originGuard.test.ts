import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { isOriginAllowed } from '../src/middleware/originGuard.js';
import { normalizeOriginString } from '../src/security/origin.js';

type Vector = { raw: string; normalized: string | null };

function readVectors(): Vector[] {
  const here = path.dirname(fileURLToPath(import.meta.url));
  const vectorsPath = path.resolve(here, '../../../docs/origin-allowlist-test-vectors.json');
  return JSON.parse(fs.readFileSync(vectorsPath, 'utf8')) as Vector[];
}

test('normalizeOriginString matches shared vectors', () => {
  for (const vector of readVectors()) {
    assert.equal(normalizeOriginString(vector.raw), vector.normalized, vector.raw);
  }
});

test('isOriginAllowed matches on normalized origin', () => {
  for (const vector of readVectors()) {
    if (vector.normalized === null) continue;
    assert.equal(isOriginAllowed(vector.raw, [vector.normalized]), true, vector.raw);
  }
});

test('isOriginAllowed handles wildcard (but still requires a valid origin)', () => {
  assert.equal(isOriginAllowed('https://evil.com', ['*']), true);
  assert.equal(isOriginAllowed('https://evil.com/path', ['*']), false);
});

test('default same-host policy normalizes IP addresses', () => {
  assert.equal(isOriginAllowed('http://010.0.0.1', [], '8.0.0.1'), true);
  assert.equal(isOriginAllowed('http://8.0.0.1', [], '010.0.0.1'), true);

  assert.equal(isOriginAllowed('http://[::FFFF:192.0.2.1]', [], '[::ffff:c000:201]'), true);
  assert.equal(isOriginAllowed('http://[::ffff:c000:201]', [], '[::FFFF:192.0.2.1]'), true);
});
