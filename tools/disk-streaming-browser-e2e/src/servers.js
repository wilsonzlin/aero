const http = require('node:http');
const { spawn } = require('node:child_process');
const fs = require('node:fs/promises');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');

const PUBLIC_IMAGE_ID = 'win7';
const PRIVATE_IMAGE_ID = 'secret';
const PRIVATE_USER_ID = 'alice';

function withCommonAppHeaders(res) {
  // Required for `window.crossOriginIsolated === true`.
  res.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
  res.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function getRepoRoot() {
  return path.join(__dirname, '..', '..', '..');
}

async function getFreePort() {
  return await new Promise((resolve, reject) => {
    const server = net.createServer();
    server.on('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address();
      server.close((err) => (err ? reject(err) : resolve(port)));
    });
  });
}

async function waitForHttpOk(url, { timeoutMs, shouldAbort }) {
  const start = Date.now();
  // Poll until the server is listening. This has to tolerate a cold `cargo run`
  // which may compile the binary first.
  while (Date.now() - start < timeoutMs) {
    if (shouldAbort) {
      const reason = shouldAbort();
      if (reason) {
        throw new Error(reason);
      }
    }
    let res;
    try {
      res = await fetch(url, { method: 'HEAD' });
    } catch {
      // connection refused / not up yet
      await sleep(100);
      continue;
    }

    if (res.ok) return;

    // We successfully connected to the server, so it is "up" but not returning
    // the expected response. That's almost always a configuration/boot failure,
    // so fail fast instead of waiting for the full timeout.
    throw new Error(`Unexpected HTTP ${res.status} from ${url} while waiting for readiness`);

    await sleep(100);
  }
  throw new Error(`Timed out waiting for ${url}`);
}

async function killChildProcess(child) {
  if (!child || child.killed || child.exitCode !== null) return;

  child.kill('SIGTERM');
  const exited = new Promise((resolve) => child.once('exit', resolve));
  await Promise.race([exited, sleep(2000)]);
  if (child.exitCode === null) {
    child.kill('SIGKILL');
    await exited;
  }
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
    const type = res.type;
    const contentRange = res.headers.get('Content-Range');
    const acceptRanges = res.headers.get('Accept-Ranges');
    const contentLength = res.headers.get('Content-Length');
    const etag = res.headers.get('ETag');
    const bytes = new Uint8Array(await res.arrayBuffer());
    assertCrossOriginIsolated('after fetchRange');
    return { status, type, headers: { contentRange, acceptRanges, contentLength, etag }, bytes };
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

    async fetchPublicRange({ imageId, start, endInclusive, expectedBytes, expectedFileSize }) {
      const url = diskOrigin + '/disk/' + encodeURIComponent(imageId);
      const { status, type, headers, bytes } = await fetchRange(url, { start, endInclusive });
      assert(status === 206, 'Expected 206 Partial Content, got ' + status);
      assert(type === 'cors', 'Expected CORS fetch response type, got ' + type);
      assert(headers.acceptRanges === 'bytes', 'Expected Accept-Ranges: bytes, got ' + headers.acceptRanges);
      assert(
        headers.contentRange === 'bytes ' + start + '-' + endInclusive + '/' + expectedFileSize,
        'Unexpected Content-Range: ' + headers.contentRange,
      );
      assert(headers.contentLength === String(expectedBytes.length), 'Unexpected Content-Length: ' + headers.contentLength);
      assert(typeof headers.etag === 'string' && headers.etag.length > 0, 'Missing ETag (and/or not exposed via CORS)');
      assertBytesEqual(bytes, expectedBytes);
    },

    async fetchPrivateRangeExpectUnauthorized({ imageId, start, endInclusive }) {
      const url = diskOrigin + '/disk/' + encodeURIComponent(imageId);
      const { status, type } = await fetchRange(url, { start, endInclusive });
      assert(status === 401, 'Expected 401 Unauthorized, got ' + status);
      assert(type === 'cors', 'Expected CORS fetch response type, got ' + type);
    },

    async fetchLeaseToken({ imageId, userId = '${PRIVATE_USER_ID}' }) {
      const url = diskOrigin + '/api/images/' + encodeURIComponent(imageId) + '/lease';
      const { status, body } = await fetchJson(url, {
        method: 'POST',
        headers: {
          // disk-gateway allows placeholder caller identity for lease issuance via
          // Authorization: Bearer <user-id>. (X-Debug-User exists too but is not
          // allowed by the server's CORS preflight.)
          Authorization: 'Bearer ' + userId,
        },
      });
      assert(status === 200, 'Expected 200 OK from lease endpoint, got ' + status);
      assert(typeof body === 'object' && body !== null && typeof body.token === 'string', 'Lease response missing { token }');
      return body.token;
    },

    async fetchPrivateRangeWithToken({ imageId, token, start, endInclusive, expectedBytes, expectedFileSize }) {
      const url = diskOrigin + '/disk/' + encodeURIComponent(imageId);
      const { status, type, headers, bytes } = await fetchRange(url, {
        start,
        endInclusive,
        headers: {
          Authorization: 'Bearer ' + token,
        },
      });
      assert(status === 206, 'Expected 206 Partial Content, got ' + status);
      assert(type === 'cors', 'Expected CORS fetch response type, got ' + type);
      assert(headers.acceptRanges === 'bytes', 'Expected Accept-Ranges: bytes, got ' + headers.acceptRanges);
      assert(
        headers.contentRange === 'bytes ' + start + '-' + endInclusive + '/' + expectedFileSize,
        'Unexpected Content-Range: ' + headers.contentRange,
      );
      assert(headers.contentLength === String(expectedBytes.length), 'Unexpected Content-Length: ' + headers.contentLength);
      assert(typeof headers.etag === 'string' && headers.etag.length > 0, 'Missing ETag (and/or not exposed via CORS)');
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

async function startDiskGatewayServer({ appOrigin, publicFixturePath, privateFixturePath }) {
  const tmpRoot = await fs.mkdtemp(path.join(os.tmpdir(), 'disk-gateway-browser-e2e-'));
  const publicDir = path.join(tmpRoot, 'public');
  const privateDir = path.join(tmpRoot, 'private');

  await fs.mkdir(publicDir, { recursive: true });
  await fs.mkdir(path.join(privateDir, PRIVATE_USER_ID), { recursive: true });

  await fs.copyFile(publicFixturePath, path.join(publicDir, `${PUBLIC_IMAGE_ID}.img`));
  await fs.copyFile(
    privateFixturePath,
    path.join(privateDir, PRIVATE_USER_ID, `${PRIVATE_IMAGE_ID}.img`),
  );

  const port = await getFreePort();
  const bind = `127.0.0.1:${port}`;
  const origin = `http://127.0.0.1:${port}`;

  const diskGatewaySourceDir = path.join(getRepoRoot(), 'server', 'disk-gateway');

  const outputLimit = 50_000;
  let output = '';
  const appendOutput = (chunk) => {
    output += chunk.toString();
    if (output.length > outputLimit) output = output.slice(-outputLimit);
  };

  /** @type {Error | null} */
  let spawnError = null;

  const child = spawn('cargo', ['run', '--locked', '--bin', 'disk-gateway'], {
    cwd: diskGatewaySourceDir,
    env: {
      ...process.env,
      DISK_GATEWAY_BIND: bind,
      DISK_GATEWAY_PUBLIC_DIR: publicDir,
      DISK_GATEWAY_PRIVATE_DIR: privateDir,
      DISK_GATEWAY_TOKEN_SECRET: 'disk-gateway-browser-e2e-secret',
      DISK_GATEWAY_CORS_ALLOWED_ORIGINS: appOrigin,
      DISK_GATEWAY_CORP: 'cross-origin',
      RUST_LOG: process.env.RUST_LOG ?? 'info',
    },
    stdio: ['ignore', 'pipe', 'pipe'],
  });

  child.stdout?.on('data', appendOutput);
  child.stderr?.on('data', appendOutput);
  child.on('error', (err) => {
    spawnError = err;
    appendOutput(`\n[disk-gateway spawn error] ${err.message}\n`);
  });

  try {
    await waitForHttpOk(`${origin}/disk/${PUBLIC_IMAGE_ID}`, {
      timeoutMs: 120_000,
      shouldAbort: () => {
        if (spawnError) {
          return `disk-gateway failed to spawn: ${spawnError.message}\n\nOutput:\n${output}`;
        }
        if (child.exitCode !== null) {
          return `disk-gateway exited early (exit ${child.exitCode}). Output:\n${output}`;
        }
        return null;
      },
    });
  } catch (err) {
    await killChildProcess(child);
    await fs.rm(tmpRoot, { recursive: true, force: true });
    const exitCode = child.exitCode;
    const msg = err instanceof Error ? err.message : String(err);
    if (msg.includes('Output:\n')) {
      throw err;
    }
    const prefix =
      exitCode === null
        ? 'disk-gateway failed to become ready.'
        : `disk-gateway failed to start (exit ${exitCode}).`;
    throw new Error(`${prefix} ${msg}\n\nOutput:\n${output}`);
  }

  return {
    origin,
    close: async () => {
      await killChildProcess(child);
      await fs.rm(tmpRoot, { recursive: true, force: true });
    },
  };
}

module.exports = {
  PRIVATE_IMAGE_ID,
  PUBLIC_IMAGE_ID,
  startAppServer,
  startDiskGatewayServer,
};
