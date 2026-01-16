const http = require('node:http');
const { spawn, spawnSync } = require('node:child_process');
const fs = require('node:fs/promises');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');

const MAX_REQUEST_URL_LEN = 8 * 1024;
const MAX_PATHNAME_LEN = 4 * 1024;
const MAX_SPAWN_ERROR_MESSAGE_BYTES = 512;
const MAX_ERROR_BODY_BYTES = 512;

const UTF8 = Object.freeze({ encoding: 'utf-8' });
const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder(UTF8.encoding);

const PUBLIC_IMAGE_ID = 'win7';
const PRIVATE_IMAGE_ID = 'secret';
const PRIVATE_USER_ID = 'alice';

function coerceString(input) {
  try {
    return String(input ?? '');
  } catch {
    return '';
  }
}

function formatOneLineUtf8(input, maxBytes) {
  if (!Number.isInteger(maxBytes) || maxBytes < 0) return '';
  if (maxBytes === 0) return '';

  const buf = new Uint8Array(maxBytes);
  let written = 0;
  let pendingSpace = false;
  for (const ch of coerceString(input)) {
    const code = ch.codePointAt(0) ?? 0;
    const forbidden = code <= 0x1f || code === 0x7f || code === 0x85 || code === 0x2028 || code === 0x2029;
    if (forbidden || /\s/u.test(ch)) {
      pendingSpace = written > 0;
      continue;
    }

    if (pendingSpace) {
      const spaceRes = textEncoder.encodeInto(' ', buf.subarray(written));
      if (spaceRes.written === 0) break;
      written += spaceRes.written;
      pendingSpace = false;
      if (written >= maxBytes) break;
    }

    const res = textEncoder.encodeInto(ch, buf.subarray(written));
    if (res.written === 0) break;
    written += res.written;
    if (written >= maxBytes) break;
  }
  return written === 0 ? '' : textDecoder.decode(buf.subarray(0, written));
}

function safeErrorMessageInput(err) {
  if (err === null) return 'null';

  const t = typeof err;
  if (t === 'string') return err;
  if (t === 'number' || t === 'boolean' || t === 'bigint' || t === 'symbol' || t === 'undefined') return String(err);

  if (t === 'object') {
    try {
      const msg = err && typeof err.message === 'string' ? err.message : null;
      if (msg !== null) return msg;
    } catch {
      // ignore getters throwing
    }
  }

  // Avoid calling toString() on arbitrary objects/functions (can throw / be expensive).
  return 'Error';
}

function formatOneLineError(err, maxBytes, fallback = 'Error') {
  const raw = safeErrorMessageInput(err);
  const safe = formatOneLineUtf8(raw, maxBytes);
  const fb = typeof fallback === 'string' && fallback ? fallback : 'Error';
  return safe || fb;
}

function withCommonAppHeaders(res) {
  // Required for `window.crossOriginIsolated === true`.
  res.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
  res.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
}

function sleep(ms) {
  return new Promise((resolve) => {
    const timeout = setTimeout(resolve, ms);
    timeout.unref?.();
  });
}

function signalProcessTree(child, signal) {
  if (!child || !child.pid) return;
  // `detached: true` makes the child process leader of a new process group (POSIX).
  // Kill the entire group so `cargo run` doesn't leak the spawned binary process.
  if (process.platform !== 'win32') {
    try {
      process.kill(-child.pid, signal);
      return;
    } catch {
      // Fall back to killing the main pid.
    }
  }
  try {
    child.kill(signal);
  } catch {
    // ignore
  }
}

function isSccacheWrapper(value) {
  if (typeof value !== 'string' || !value) return false;
  const v = value.toLowerCase();
  return (
    v === 'sccache' ||
    v === 'sccache.exe' ||
    v.endsWith('/sccache') ||
    v.endsWith('\\sccache') ||
    v.endsWith('/sccache.exe') ||
    v.endsWith('\\sccache.exe')
  );
}

let rustcHostTargetCache;
function rustcHostTarget() {
  if (rustcHostTargetCache !== undefined) return rustcHostTargetCache;
  try {
    const vv = spawnSync('rustc', ['-vV'], {
      cwd: getRepoRoot(),
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
      timeout: 5_000,
    });
    if (vv.status !== 0) {
      rustcHostTargetCache = null;
      return rustcHostTargetCache;
    }
    const m = (vv.stdout ?? '').match(/^host:\s*(.+)\s*$/m);
    rustcHostTargetCache = m ? m[1].trim() : null;
    return rustcHostTargetCache;
  } catch {
    rustcHostTargetCache = null;
    return rustcHostTargetCache;
  }
}

function cargoTargetRustflagsVar(target) {
  return `CARGO_TARGET_${target.toUpperCase().replace(/[-.]/g, '_')}_RUSTFLAGS`;
}

function parsePositiveIntEnv(value) {
  if (typeof value !== 'string') return null;
  if (!/^[1-9][0-9]*$/.test(value)) return null;
  const n = Number.parseInt(value, 10);
  if (!Number.isFinite(n) || !Number.isSafeInteger(n) || n <= 0) return null;
  return n;
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
  function coerceUrlString(u) {
    if (typeof u === 'string') return u;
    if (u instanceof URL) return u.toString();
    if (typeof u === 'number' || typeof u === 'boolean' || typeof u === 'bigint') return String(u);
    return '';
  }

  function formatUrlForError(u) {
    const raw = coerceUrlString(u);
    try {
      const parsed = new URL(raw);
      return `${parsed.origin}${parsed.pathname}`;
    } catch {
      const s = raw || 'invalid url';
      return s.length > 128 ? `${s.slice(0, 128)}…(${s.length} chars)` : s;
    }
  }

  const start = Date.now();
  // Poll until the server is listening. This has to tolerate a cold `cargo run --locked`
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
    throw new Error(`Unexpected HTTP ${res.status} while waiting for readiness (${formatUrlForError(url)})`);
  }
  throw new Error(`Timed out waiting for ${formatUrlForError(url)}`);
}

async function killChildProcess(child) {
  if (!child || child.killed || child.exitCode !== null) return;

  signalProcessTree(child, 'SIGTERM');
  const exited = new Promise((resolve) => {
    child.once('exit', resolve);
    child.once('close', resolve);
  });
  await Promise.race([exited, sleep(2000)]);
  if (child.exitCode === null) {
    signalProcessTree(child, 'SIGKILL');
    await Promise.race([exited, sleep(2000)]);
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

    async fetchPrivateRangeWithQueryToken({ imageId, token, start, endInclusive, expectedBytes, expectedFileSize }) {
      const url =
        diskOrigin + '/disk/' + encodeURIComponent(imageId) + '?token=' + encodeURIComponent(token);
      const { status, type, headers, bytes } = await fetchRange(url, {
        start,
        endInclusive,
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

function sendText(res, statusCode, text) {
  res.statusCode = statusCode;
  res.setHeader('Content-Type', 'text/plain; charset=utf-8');
  const safeText = formatOneLineUtf8(text, MAX_ERROR_BODY_BYTES) || 'Error';
  res.end(safeText);
}

async function startAppServer() {
  const server = http.createServer((req, res) => {
    withCommonAppHeaders(res);

    const rawUrl = req.url ?? '/';
    if (typeof rawUrl !== 'string') {
      sendText(res, 400, 'Bad Request');
      return;
    }
    if (rawUrl.length > MAX_REQUEST_URL_LEN) {
      sendText(res, 414, 'URI Too Long');
      return;
    }

    let url;
    try {
      // Never use attacker-controlled Host header as a URL parsing base.
      url = new URL(rawUrl, 'http://localhost');
    } catch {
      sendText(res, 400, 'Bad Request');
      return;
    }
    if (url.pathname.length > MAX_PATHNAME_LEN) {
      sendText(res, 414, 'URI Too Long');
      return;
    }

    if (req.method === 'GET' && url.pathname === '/') {
      const html = renderIndexHtml();
      res.statusCode = 200;
      res.setHeader('Content-Type', 'text/html; charset=utf-8');
      res.end(html);
      return;
    }

    // Browsers often probe for /favicon.ico, etc. Ensure COOP/COEP are still
    // present on these responses to keep the surface area realistic.
    sendText(res, 404, 'not found');
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
    if (typeof chunk === 'string') {
      output += chunk;
    } else if (Buffer.isBuffer(chunk)) {
      output += chunk.toString('utf8');
    } else if (chunk instanceof Uint8Array) {
      output += Buffer.from(chunk).toString('utf8');
    } else {
      return;
    }
    if (output.length > outputLimit) output = output.slice(-outputLimit);
  };

  /** @type {Error | null} */
  let spawnError = null;

  const env = {
    ...process.env,
    // Build disk-gateway into the repo-level target dir so CI's rust-cache
    // (which caches `${repo}/target`) can reuse compilation artifacts.
    //
    // This also avoids rebuilding disk-gateway from scratch when the harness
    // is run repeatedly during local development.
    CARGO_TARGET_DIR:
      process.env.CARGO_TARGET_DIR ?? path.join(getRepoRoot(), 'target', 'disk-gateway-e2e'),
    DISK_GATEWAY_BIND: bind,
    DISK_GATEWAY_PUBLIC_DIR: publicDir,
    DISK_GATEWAY_PRIVATE_DIR: privateDir,
    DISK_GATEWAY_TOKEN_SECRET: 'disk-gateway-browser-e2e-secret',
    DISK_GATEWAY_CORS_ALLOWED_ORIGINS: appOrigin,
    DISK_GATEWAY_CORP: 'cross-origin',
    RUST_LOG: process.env.RUST_LOG ?? 'info',
  };

  // Defensive defaults for thread-limited environments:
  // - Cargo defaults to one build job per CPU core, which can spawn many rustc processes in
  //   parallel.
  // - rustc also has internal worker pools (including Rayon) which default to `num_cpus`.
  //
  // When running these Playwright tests in constrained CI sandboxes, those defaults can exceed
  // per-user thread/PID limits and cause intermittent EAGAIN/WouldBlock failures.
  //
  // Prefer a conservative default of -j1 unless the caller explicitly overrides via either the
  // canonical agent knob `AERO_CARGO_BUILD_JOBS` or a valid `CARGO_BUILD_JOBS`.
  const defaultJobs = 1;
  const jobsFromAero = parsePositiveIntEnv(env.AERO_CARGO_BUILD_JOBS);
  if (jobsFromAero !== null) {
    env.CARGO_BUILD_JOBS = String(jobsFromAero);
  } else if (parsePositiveIntEnv(env.CARGO_BUILD_JOBS) === null) {
    env.CARGO_BUILD_JOBS = String(defaultJobs);
  }
  const jobs = parsePositiveIntEnv(env.CARGO_BUILD_JOBS) ?? defaultJobs;

  if (parsePositiveIntEnv(env.RUSTC_WORKER_THREADS) === null) {
    env.RUSTC_WORKER_THREADS = String(jobs);
  }
  if (parsePositiveIntEnv(env.RAYON_NUM_THREADS) === null) {
    env.RAYON_NUM_THREADS = String(jobs);
  }
  if (parsePositiveIntEnv(env.AERO_TOKIO_WORKER_THREADS) === null) {
    env.AERO_TOKIO_WORKER_THREADS = String(jobs);
  }

  // Limit LLVM lld thread parallelism on Linux (matches safe-run/agent-env behavior).
  // Use Cargo's per-target rustflags env var rather than mutating global RUSTFLAGS so this
  // doesn't leak into wasm builds (rust-lld -flavor wasm does not understand -Wl,...).
  if (process.platform === 'linux') {
    const host = rustcHostTarget();
    if (host) {
      const varName = cargoTargetRustflagsVar(host);
      const current = env[varName] ?? '';
      if (!current.includes('--threads=') && !current.includes('-Wl,--threads=')) {
        env[varName] = `${current} -C link-arg=-Wl,--threads=${jobs}`.trim();
      }
    }
  }

  // Some environments configure a rustc wrapper (e.g. `sccache`) via global Cargo config.
  // That can make this harness flaky when the wrapper isn't available. Detect `sccache` wrappers
  // and override them.
  const wrapperVars = [
    'RUSTC_WRAPPER',
    'RUSTC_WORKSPACE_WRAPPER',
    'CARGO_BUILD_RUSTC_WRAPPER',
    'CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER',
  ];
  const usesSccache = wrapperVars.some((k) => isSccacheWrapper(env[k]));
  const hasWrapper = wrapperVars.some((k) => Object.prototype.hasOwnProperty.call(env, k));
  if (usesSccache || !hasWrapper) {
    env.RUSTC_WRAPPER = '';
    env.RUSTC_WORKSPACE_WRAPPER = '';
    env.CARGO_BUILD_RUSTC_WRAPPER = '';
    env.CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER = '';
  }

  // Allow running these e2e helpers with a per-checkout Cargo home without having to
  // source `scripts/agent-env.sh` first.
  if (process.env.AERO_ISOLATE_CARGO_HOME && !('CARGO_HOME' in env)) {
    env.CARGO_HOME = path.join(getRepoRoot(), '.cargo-home');
    await fs.mkdir(env.CARGO_HOME, { recursive: true });
  }

  const child = spawn('cargo', ['run', '--locked', '--bin', 'disk-gateway'], {
    cwd: diskGatewaySourceDir,
    env,
    detached: process.platform !== 'win32',
    stdio: ['ignore', 'pipe', 'pipe'],
  });

  child.stdout?.on('data', appendOutput);
  child.stderr?.on('data', appendOutput);
  child.on('error', (err) => {
    spawnError = err;
    const msg = formatOneLineError(err, MAX_SPAWN_ERROR_MESSAGE_BYTES);
    appendOutput(`\n[disk-gateway spawn error] ${msg}\n`);
  });

  try {
    await waitForHttpOk(`${origin}/disk/${PUBLIC_IMAGE_ID}`, {
      timeoutMs: 120_000,
      shouldAbort: () => {
        if (spawnError) {
          const msg = formatOneLineError(spawnError, MAX_SPAWN_ERROR_MESSAGE_BYTES);
          return `disk-gateway failed to spawn: ${msg}\n\nOutput:\n${output}`;
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
    const msgForOutputCheck = typeof err === 'string' ? err : safeErrorMessageInput(err);
    if (msgForOutputCheck.includes('Output:\n')) {
      throw err;
    }
    const msg = formatOneLineError(err, MAX_SPAWN_ERROR_MESSAGE_BYTES);
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
