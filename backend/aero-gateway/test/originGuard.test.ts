import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { isOriginAllowed, originGuard } from '../src/middleware/originGuard.js';
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

test('normalizeOriginString rejects oversized Origin headers', () => {
  const origin = `https://${'a'.repeat(10_000)}.example`;
  assert.equal(normalizeOriginString(origin), null);
  assert.equal(isOriginAllowed(origin, ['*']), false);
});

test('default same-host policy normalizes IP addresses', () => {
  assert.equal(isOriginAllowed('http://010.0.0.1', [], '8.0.0.1'), true);
  assert.equal(isOriginAllowed('http://8.0.0.1', [], '010.0.0.1'), true);

  assert.equal(isOriginAllowed('http://[::FFFF:192.0.2.1]', [], '[::ffff:c000:201]'), true);
  assert.equal(isOriginAllowed('http://[::ffff:c000:201]', [], '[::FFFF:192.0.2.1]'), true);
});

test('default same-host policy rejects oversized Host headers', () => {
  const host = 'a'.repeat(10_000);
  assert.equal(isOriginAllowed('https://example.com', [], host), false);
});

test('originGuard treats repeated Host headers as invalid under default same-host policy', async () => {
  const reply: {
    status?: number;
    body?: unknown;
    headers: Record<string, string>;
    code: (n: number) => typeof reply;
    send: (body: unknown) => typeof reply;
    header: (key: string, value: string) => typeof reply;
  } = {
    headers: {},
    code: (n) => {
      reply.status = n;
      return reply;
    },
    send: (body) => {
      reply.body = body;
      return reply;
    },
    header: (key, value) => {
      reply.headers[key.toLowerCase()] = value;
      return reply;
    },
  };

  const request = {
    method: 'GET',
    headers: {
      origin: 'https://example.com',
      host: ['example.com', 'evil.example'],
    },
  } as unknown as Parameters<typeof originGuard>[0];

  await originGuard(request, reply as unknown as Parameters<typeof originGuard>[1], { allowedOrigins: [] });
  assert.equal(reply.status, 403);
  assert.deepEqual(reply.body, { error: 'forbidden', message: 'Origin not allowed' });
  assert.equal(reply.headers['access-control-allow-origin'], undefined);
});

test('originGuard rejects repeated Origin headers via rawHeaders (even if req.headers.origin is a string)', async () => {
  const reply: {
    status?: number;
    body?: unknown;
    headers: Record<string, string>;
    code: (n: number) => typeof reply;
    send: (body: unknown) => typeof reply;
    header: (key: string, value: string) => typeof reply;
  } = {
    headers: {},
    code: (n) => {
      reply.status = n;
      return reply;
    },
    send: (body) => {
      reply.body = body;
      return reply;
    },
    header: (key, value) => {
      reply.headers[key.toLowerCase()] = value;
      return reply;
    },
  };

  const request = {
    method: 'GET',
    headers: {
      origin: 'https://example.com',
      host: 'example.com',
    },
    raw: {
      rawHeaders: [
        'Origin',
        'https://example.com',
        'Origin',
        'https://evil.example',
        'Host',
        'example.com',
      ],
    },
  } as unknown as Parameters<typeof originGuard>[0];

  await originGuard(request, reply as unknown as Parameters<typeof originGuard>[1], { allowedOrigins: ['*'] });
  assert.equal(reply.status, 403);
  assert.deepEqual(reply.body, { error: 'forbidden', message: 'Origin not allowed' });
  assert.equal(reply.headers['access-control-allow-origin'], undefined);
});

test('originGuard treats repeated Host headers in rawHeaders as invalid under default same-host policy', async () => {
  const reply: {
    status?: number;
    body?: unknown;
    headers: Record<string, string>;
    code: (n: number) => typeof reply;
    send: (body: unknown) => typeof reply;
    header: (key: string, value: string) => typeof reply;
  } = {
    headers: {},
    code: (n) => {
      reply.status = n;
      return reply;
    },
    send: (body) => {
      reply.body = body;
      return reply;
    },
    header: (key, value) => {
      reply.headers[key.toLowerCase()] = value;
      return reply;
    },
  };

  const request = {
    method: 'GET',
    headers: {
      origin: 'https://example.com',
      host: 'example.com',
    },
    raw: {
      rawHeaders: [
        'Origin',
        'https://example.com',
        'Host',
        'example.com',
        'Host',
        'evil.example',
      ],
    },
  } as unknown as Parameters<typeof originGuard>[0];

  await originGuard(request, reply as unknown as Parameters<typeof originGuard>[1], { allowedOrigins: [] });
  assert.equal(reply.status, 403);
  assert.deepEqual(reply.body, { error: 'forbidden', message: 'Origin not allowed' });
  assert.equal(reply.headers['access-control-allow-origin'], undefined);
});

test('originGuard preflight does not echo oversized access-control-request-headers', async () => {
  const reply: {
    status?: number;
    body?: unknown;
    headers: Record<string, string>;
    code: (n: number) => typeof reply;
    send: (body: unknown) => typeof reply;
    header: (key: string, value: string) => typeof reply;
  } = {
    headers: {},
    code: (n) => {
      reply.status = n;
      return reply;
    },
    send: (body) => {
      reply.body = body;
      return reply;
    },
    header: (key, value) => {
      reply.headers[key.toLowerCase()] = value;
      return reply;
    },
  };

  const request = {
    method: 'OPTIONS',
    headers: {
      origin: 'https://example.com',
      host: 'example.com',
      'access-control-request-method': 'POST',
      'access-control-request-headers': `content-type, ${'x'.repeat(10_000)}`,
    },
  } as unknown as Parameters<typeof originGuard>[0];

  await originGuard(request, reply as unknown as Parameters<typeof originGuard>[1], { allowedOrigins: [] });
  assert.equal(reply.status, 204);
  assert.equal(reply.headers['access-control-allow-origin'], 'https://example.com');
  assert.equal(reply.headers['access-control-allow-headers'], 'Content-Type');
});
