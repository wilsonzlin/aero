import assert from 'node:assert/strict';
import test from 'node:test';
import { buildServer } from '../src/server.js';
import { makeTestConfig } from './testConfig.js';

const baseConfig = makeTestConfig({
  TCP_ALLOW_PRIVATE_IPS: false,
  TCP_PROXY_MAX_CONNECTIONS: 64,
});

test('/dns-query requires a valid aero_session cookie', async () => {
  const { app } = buildServer(baseConfig);
  await app.ready();

  const dns = 'AAABAAABAAAAAAAAB2V4YW1wbGUDY29tAAABAAE'; // example.com A, RFC8484 base64url

  const unauth = await app.inject({ method: 'GET', url: `/dns-query?dns=${dns}` });
  assert.equal(unauth.statusCode, 401);

  const sessionRes = await app.inject({ method: 'POST', url: '/session' });
  assert.equal(sessionRes.statusCode, 201);
  const setCookie = sessionRes.headers['set-cookie'];
  assert.ok(setCookie, 'expected Set-Cookie header');
  const cookie = (Array.isArray(setCookie) ? setCookie[0] : setCookie).split(';')[0]!;

  const auth = await app.inject({ method: 'GET', url: `/dns-query?dns=${dns}`, headers: { cookie } });
  assert.equal(auth.statusCode, 200);
  const contentType = auth.headers['content-type'];
  const contentTypeStr =
    typeof contentType === 'string'
      ? contentType
      : Array.isArray(contentType)
        ? contentType.join(',')
        : String(contentType ?? '');
  assert.ok(contentTypeStr.startsWith('application/dns-message'));

  await app.close();
});
