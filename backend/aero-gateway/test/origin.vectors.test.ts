import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { describe, it } from 'node:test';
import { fileURLToPath } from 'node:url';

import { isOriginAllowed } from '../src/middleware/originGuard.js';
import { normalizeOriginString } from '../src/security/origin.js';

type OriginVectorsFile = {
  schema: number;
  normalize: Array<
    | { name: string; rawOriginHeader: string; normalizedOrigin: string }
    | { name: string; rawOriginHeader: string; expectError: true }
  >;
  allow: Array<{
    name: string;
    allowedOrigins: string[];
    requestHost: string;
    rawOriginHeader: string;
    expectAllowed: boolean;
  }>;
};

function vectorsPath(): string {
  const dir = path.dirname(fileURLToPath(import.meta.url));
  return path.resolve(dir, '..', '..', '..', 'protocol-vectors', 'origin.json');
}

describe('origin vectors', () => {
  const vectors = JSON.parse(fs.readFileSync(vectorsPath(), 'utf8')) as OriginVectorsFile;
  assert.equal(vectors.schema, 1);

  for (const v of vectors.normalize) {
    it(`normalize/${v.name}`, () => {
      const normalized = normalizeOriginString(v.rawOriginHeader);
      if ('expectError' in v) {
        assert.equal(normalized, null);
        return;
      }

      assert.equal(normalized ?? null, v.normalizedOrigin);
    });
  }

  for (const v of vectors.allow) {
    it(`allow/${v.name}`, () => {
      assert.equal(isOriginAllowed(v.rawOriginHeader, v.allowedOrigins, v.requestHost), v.expectAllowed);
    });
  }
});
