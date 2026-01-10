const http = require('node:http');
const { readFile } = require('node:fs/promises');

const PUBLIC_IMAGE_ID = 'public-fixture';
const PRIVATE_IMAGE_ID = 'private-fixture';

function withCommonAppHeaders(res) {
  // Required for `window.crossOriginIsolated === true`.
  res.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
  res.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
}

function withCommonDiskHeaders(req, res) {
  // The COEP page will fetch cross-origin resources. Under `COEP: require-corp`,
  // the resource must be CORS-enabled and/or explicitly CORP-allowed.
  res.setHeader('Access-Control-Allow-Origin', '*');
  res.setHeader('Access-Control-Allow-Methods', 'GET, OPTIONS');
  res.setHeader('Access-Control-Allow-Headers', 'Authorization, Range, Content-Type');
  res.setHeader('Access-Control-Expose-Headers', 'Content-Range, Accept-Ranges');
  res.setHeader('Cross-Origin-Resource-Policy', 'cross-origin');

  // Avoid caches hiding header regressions.
  res.setHeader('Cache-Control', 'no-store');

  // Keep intermediaries honest if they vary by Origin later.
  if (req.headers.origin) {
    res.setHeader('Vary', 'Origin');
  }
}

function parseBearerToken(authorizationHeader) {
  if (!authorizationHeader) return null;
  const match = /^\s*Bearer\s+(.+?)\s*$/.exec(authorizationHeader);
  return match ? match[1] : null;
}

function parseRangeHeader(rangeHeader, size) {
  if (!rangeHeader) return null;
  const match = /^bytes=(\d+)-(\d+)?$/.exec(rangeHeader);
  if (!match) return null;
  const start = Number(match[1]);
  const endInclusive = match[2] === undefined ? size - 1 : Number(match[2]);
  if (!Number.isFinite(start) || !Number.isFinite(endInclusive)) return null;
  if (start < 0 || endInclusive < start || start >= size) return null;
  return { start, endInclusive: Math.min(endInclusive, size - 1) };
}

function renderIndexHtml() {
  // This page intentionally hosts the “assertions” that HTTP-only tests can't see:
  // crossOriginIsolated state and browser-enforced COEP behavior.
  return `<!doctype html>
<meta charset="utf-8">
<title>disk streaming browser e2e</title>
<script>
(() => {
  const params = new URLSearchParams(location.search);
  const diskOrigin = params.get('diskOrigin');
  if (!diskOrigin) throw new Error('Missing required ?diskOrigin=');

  function assert(condition, message) {
    if (!condition) throw new Error(message);
  }

  function assertCrossOriginIsolated(where) {
    assert(window.crossOriginIsolated === true, where + ': window.crossOriginIsolated should be true');
  }

  async function fetchJson(url, init) {
    assertCrossOriginIsolated('before fetchJson');
    const res = await fetch(url, init);
    assertCrossOriginIsolated('after fetchJson');
    const body = await res.json();
    return { status: res.status, body };
  }

  async function fetchRange(url, { start, endInclusive, headers = {} }) {
    assertCrossOriginIsolated('before fetchRange');
    const res = await fetch(url, {
      headers: {
        ...headers,
        Range: 'bytes=' + start + '-' + endInclusive,
      },
    });
    const status = res.status;
    const bytes = new Uint8Array(await res.arrayBuffer());
    assertCrossOriginIsolated('after fetchRange');
    return { status, bytes };
  }

  function assertBytesEqual(actualU8, expectedArray) {
    assert(actualU8.length === expectedArray.length, 'Expected ' + expectedArray.length + ' bytes, got ' + actualU8.length);
    for (let i = 0; i < expectedArray.length; i++) {
      if (actualU8[i] !== expectedArray[i]) {
        throw new Error('Byte mismatch at offset ' + i + ': expected ' + expectedArray[i] + ', got ' + actualU8[i]);
      }
    }
  }

  window.__diskStreamingE2E = {
    diskOrigin,

    assertCrossOriginIsolated() {
      assertCrossOriginIsolated('assertCrossOriginIsolated');
    },

    async fetchPublicRange({ imageId, start, endInclusive, expectedBytes }) {
      const url = diskOrigin + '/api/images/' + encodeURIComponent(imageId) + '/bytes';
      const { status, bytes } = await fetchRange(url, { start, endInclusive });
      assert(status === 206, 'Expected 206 Partial Content, got ' + status);
      assertBytesEqual(bytes, expectedBytes);
    },

    async fetchPrivateRangeExpectUnauthorized({ imageId, start, endInclusive }) {
      const url = diskOrigin + '/api/images/' + encodeURIComponent(imageId) + '/bytes';
      const { status } = await fetchRange(url, { start, endInclusive });
      assert(status === 401, 'Expected 401 Unauthorized, got ' + status);
    },

    async fetchLeaseToken({ imageId }) {
      const url = diskOrigin + '/api/images/' + encodeURIComponent(imageId) + '/lease';
      const { status, body } = await fetchJson(url);
      assert(status === 200, 'Expected 200 OK from lease endpoint, got ' + status);
      assert(typeof body === 'object' && body !== null && typeof body.token === 'string', 'Lease response missing { token }');
      return body.token;
    },

    async fetchPrivateRangeWithToken({ imageId, token, start, endInclusive, expectedBytes }) {
      const url = diskOrigin + '/api/images/' + encodeURIComponent(imageId) + '/bytes';
      const { status, bytes } = await fetchRange(url, {
        start,
        endInclusive,
        headers: {
          Authorization: 'Bearer ' + token,
        },
      });
      assert(status === 206, 'Expected 206 Partial Content, got ' + status);
      assertBytesEqual(bytes, expectedBytes);
    },
  };

  // Basic sanity check on load so failures are obvious in the browser console.
  assertCrossOriginIsolated('onload');
})();
</script>
`;
}

async function startAppServer() {
  const server = http.createServer((req, res) => {
    withCommonAppHeaders(res);

    const url = new URL(req.url, `http://${req.headers.host}`);

    if (req.method === 'GET' && url.pathname === '/') {
      const html = renderIndexHtml();
      res.statusCode = 200;
      res.setHeader('Content-Type', 'text/html; charset=utf-8');
      res.end(html);
      return;
    }

    // Browsers often probe for /favicon.ico, etc. Ensure COOP/COEP are still
    // present on these responses to keep the surface area realistic.
    res.statusCode = 404;
    res.setHeader('Content-Type', 'text/plain; charset=utf-8');
    res.end('not found');
  });

  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const { port } = server.address();
  return {
    origin: `http://127.0.0.1:${port}`,
    close: () => new Promise((resolve, reject) => server.close((err) => (err ? reject(err) : resolve()))),
  };
}

async function startDiskGatewayStubServer({ publicFixturePath, privateFixturePath }) {
  const publicBytes = await readFile(publicFixturePath);
  const privateBytes = await readFile(privateFixturePath);

  const privateToken = 'test-private-lease-token';

  const server = http.createServer((req, res) => {
    withCommonDiskHeaders(req, res);

    const url = new URL(req.url, `http://${req.headers.host}`);

    if (req.method === 'OPTIONS') {
      // CORS preflight (Range and/or Authorization triggers this).
      const requestedHeaders = req.headers['access-control-request-headers'];
      if (requestedHeaders) {
        res.setHeader('Access-Control-Allow-Headers', requestedHeaders);
      }
      res.statusCode = 204;
      res.end();
      return;
    }

    const leaseMatch = /^\/api\/images\/([^/]+)\/lease$/.exec(url.pathname);
    if (req.method === 'GET' && leaseMatch) {
      const imageId = decodeURIComponent(leaseMatch[1]);
      if (imageId !== PRIVATE_IMAGE_ID) {
        res.statusCode = 404;
        res.setHeader('Content-Type', 'application/json; charset=utf-8');
        res.end(JSON.stringify({ error: 'unknown image id' }));
        return;
      }

      res.statusCode = 200;
      res.setHeader('Content-Type', 'application/json; charset=utf-8');
      res.end(JSON.stringify({ token: privateToken }));
      return;
    }

    const bytesMatch = /^\/api\/images\/([^/]+)\/bytes$/.exec(url.pathname);
    if (req.method === 'GET' && bytesMatch) {
      const imageId = decodeURIComponent(bytesMatch[1]);

      let fileBytes;
      if (imageId === PUBLIC_IMAGE_ID) {
        fileBytes = publicBytes;
      } else if (imageId === PRIVATE_IMAGE_ID) {
        const providedToken =
          parseBearerToken(req.headers.authorization) ?? url.searchParams.get('token');
        if (providedToken !== privateToken) {
          res.statusCode = 401;
          res.setHeader('Content-Type', 'application/json; charset=utf-8');
          res.end(JSON.stringify({ error: 'missing or invalid token' }));
          return;
        }
        fileBytes = privateBytes;
      } else {
        res.statusCode = 404;
        res.setHeader('Content-Type', 'application/json; charset=utf-8');
        res.end(JSON.stringify({ error: 'unknown image id' }));
        return;
      }

      res.setHeader('Accept-Ranges', 'bytes');
      res.setHeader('Content-Type', 'application/octet-stream');

      const range = parseRangeHeader(req.headers.range, fileBytes.length);
      if (!range) {
        res.statusCode = 416;
        res.setHeader('Content-Type', 'application/json; charset=utf-8');
        res.end(JSON.stringify({ error: 'missing/invalid Range header' }));
        return;
      }

      const { start, endInclusive } = range;
      const chunk = fileBytes.subarray(start, endInclusive + 1);

      res.statusCode = 206;
      res.setHeader('Content-Length', String(chunk.length));
      res.setHeader('Content-Range', `bytes ${start}-${endInclusive}/${fileBytes.length}`);
      res.end(chunk);
      return;
    }

    res.statusCode = 404;
    res.setHeader('Content-Type', 'application/json; charset=utf-8');
    res.end(JSON.stringify({ error: 'not found' }));
  });

  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const { port } = server.address();
  return {
    origin: `http://127.0.0.1:${port}`,
    close: () => new Promise((resolve, reject) => server.close((err) => (err ? reject(err) : resolve()))),
  };
}

module.exports = {
  PRIVATE_IMAGE_ID,
  PUBLIC_IMAGE_ID,
  startAppServer,
  startDiskGatewayStubServer,
};

