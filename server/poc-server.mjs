import http from 'node:http';
import { readFile } from 'node:fs/promises';
import { createReadStream, statSync } from 'node:fs';
import path from 'node:path';
import { pipeline } from 'node:stream/promises';
import { fileURLToPath } from 'node:url';
import { formatOneLineError, formatOneLineUtf8 } from './src/text.js';
import { isExpectedStreamAbort } from '../src/stream_abort.js';
import { tryGetProp } from '../src/safe_props.js';

const MAX_REQUEST_URL_LEN = 8 * 1024;
const MAX_PATHNAME_LEN = 4 * 1024;
const MAX_ERROR_BODY_BYTES = 512;

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const repoRoot = path.resolve(__dirname, '..');
const webPublicRoot = path.join(repoRoot, 'web', 'public');
const webDistRoot = path.join(repoRoot, 'web', 'dist');
const coopCoepSetting = String(process.env.VITE_DISABLE_COOP_COEP ?? '').toLowerCase();
const coopCoepDisabled = coopCoepSetting === '1' || coopCoepSetting === 'true';

function parsePort() {
  const args = process.argv.slice(2);
  const portIdx = args.findIndex((arg) => arg === '--port');
  if (portIdx !== -1) {
    const val = Number.parseInt(args[portIdx + 1] ?? '', 10);
    if (Number.isFinite(val) && val > 0) return val;
  }
  const envVal = Number.parseInt(process.env.PORT ?? '4173', 10);
  if (Number.isFinite(envVal) && envVal > 0) return envVal;
  return 4173;
}

const port = parsePort();

/**
 * CSP modes:
 * - strict: no 'unsafe-eval' and no 'wasm-unsafe-eval'
 * - wasm-unsafe-eval: allows dynamic wasm compilation without enabling JS eval
 * - unsafe-eval: allows both JS eval and dynamic wasm compilation (for older/quirky engines)
 */
function cspHeader(mode) {
  const base = [
    "default-src 'self'",
    // NOTE: We intentionally do *not* include 'unsafe-inline'.
    // The PoC uses only external scripts.
    "script-src 'self'",
    "object-src 'none'",
    "base-uri 'none'",
    "frame-ancestors 'none'",
  ];
  if (mode === 'wasm-unsafe-eval') {
    base[1] = "script-src 'self' 'wasm-unsafe-eval'";
  } else if (mode === 'unsafe-eval') {
    base[1] = "script-src 'self' 'unsafe-eval'";
  }
  return base.join('; ');
}

function withCommonHeaders(res) {
  try {
    if (!coopCoepDisabled) {
      // COOP/COEP are required for SharedArrayBuffer + threads and (in Chrome) more accurate memory measurement APIs.
      res.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
      res.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
      res.setHeader('Cross-Origin-Resource-Policy', 'same-origin');
      res.setHeader('Origin-Agent-Cluster', '?1');
    }
    res.setHeader('Cache-Control', 'no-store');
    return true;
  } catch {
    try {
      res.destroy();
    } catch {
      // ignore
    }
    return false;
  }
}

function pipeFile(res, filePath) {
  const stream = createReadStream(filePath);
  void pipeline(stream, res).catch((err) => {
    if (isExpectedStreamAbort(err)) return;
    // eslint-disable-next-line no-console
    console.error(`poc-server: stream error: ${formatOneLineError(err, 512, 'Error')}`);
    // Defensive: avoid reading response state getters in error paths. Best-effort emit a stable
    // 500; `sendText` will destroy on write failure.
    sendText(res, 500, 'Internal server error');
  });
}

function contentTypeFor(filePath) {
  if (filePath.endsWith('.html')) return 'text/html; charset=utf-8';
  if (filePath.endsWith('.js')) return 'text/javascript; charset=utf-8';
  if (filePath.endsWith('.map')) return 'application/json; charset=utf-8';
  if (filePath.endsWith('.wasm')) return 'application/wasm';
  if (filePath.endsWith('.css')) return 'text/css; charset=utf-8';
  return 'application/octet-stream';
}

function isPathInside(parent, child) {
  const rel = path.relative(parent, child);
  return rel && !rel.startsWith('..') && !path.isAbsolute(rel);
}

function tryStatFile(filePath) {
  try {
    const stat = statSync(filePath);
    return stat.isFile() ? stat : null;
  } catch {
    return null;
  }
}

async function handleIndex(res, mode) {
  const html = await readFile(path.join(webPublicRoot, 'wasm-jit-csp', 'index.html'), 'utf8');
  const bytes = Buffer.from(html, 'utf8');
  if (!withCommonHeaders(res)) return;
  try {
    res.setHeader('Content-Security-Policy', cspHeader(mode));
    res.writeHead(200, {
      'Content-Type': 'text/html; charset=utf-8',
      'Content-Length': String(bytes.byteLength),
    });
    res.end(bytes);
  } catch {
    try {
      res.destroy();
    } catch {
      // ignore
    }
  }
}

function handleStaticFile(reqPath, res) {
  let decodedPath;
  try {
    decodedPath = decodeURIComponent(reqPath);
  } catch {
    return null;
  }
  // `reqPath` is already capped, but percent-decoding can expand it.
  if (decodedPath.length > MAX_PATHNAME_LEN) {
    return null;
  }
  if (decodedPath.includes('\0')) {
    return null;
  }

  // Serve built browser JS.
  if (decodedPath.startsWith('/dist/')) {
    const filePath = path.join(webDistRoot, decodedPath.slice('/dist/'.length));
    if (!isPathInside(webDistRoot, filePath)) return false;
    const stat = tryStatFile(filePath);
    if (!stat) return false;
    if (!withCommonHeaders(res)) return true;
    try {
      res.writeHead(200, {
        'Content-Type': contentTypeFor(filePath),
        'Content-Length': String(stat.size),
      });
    } catch {
      try {
        res.destroy();
      } catch {
        // ignore
      }
      return true;
    }
    pipeFile(res, filePath);
    return true;
  }

  // Serve static assets (including the precompiled mega-module).
  // `reqPath` always begins with `/`, which would otherwise cause `path.join(...)`
  // to treat it as absolute and ignore the root prefix.
  const publicRelPath = decodedPath.replace(/^\/+/, '');
  if (publicRelPath) {
    const filePath = path.join(webPublicRoot, publicRelPath);
    if (isPathInside(webPublicRoot, filePath)) {
      const stat = tryStatFile(filePath);
      if (!stat) return false;
      if (!withCommonHeaders(res)) return true;
      try {
        res.writeHead(200, {
          'Content-Type': contentTypeFor(filePath),
          'Content-Length': String(stat.size),
        });
      } catch {
        try {
          res.destroy();
        } catch {
          // ignore
        }
        return true;
      }
      pipeFile(res, filePath);
      return true;
    }
  }

  return false;
}

function sendText(res, statusCode, message) {
  if (!withCommonHeaders(res)) return;
  const safeMessage = formatOneLineUtf8(message, MAX_ERROR_BODY_BYTES) || 'Error';
  const bytes = Buffer.from(safeMessage, 'utf8');
  try {
    res.writeHead(statusCode, {
      'Content-Type': 'text/plain; charset=utf-8',
      'Content-Length': String(bytes.byteLength),
    });
    res.end(bytes);
  } catch {
    try {
      res.destroy();
    } catch {
      // ignore
    }
  }
}

const server = http.createServer(async (req, res) => {
  try {
    const rawUrl = tryGetProp(req, 'url');
    if (typeof rawUrl !== 'string' || rawUrl === '') {
      sendText(res, 400, 'Bad Request');
      return;
    }
    if (rawUrl.length > MAX_REQUEST_URL_LEN) {
      sendText(res, 414, 'URI Too Long');
      return;
    }

    let url;
    try {
      url = new URL(rawUrl, 'http://localhost');
    } catch {
      sendText(res, 400, 'Bad Request');
      return;
    }
    const reqPath = url.pathname;
    if (reqPath.length > MAX_PATHNAME_LEN) {
      sendText(res, 414, 'URI Too Long');
      return;
    }

    // CSP test entry points.
    if (reqPath === '/' || reqPath === '/index.html') {
      const body = `<!doctype html>
<meta charset="utf-8">
<title>Aero WASM JIT CSP PoC</title>
<h1>Aero WASM JIT CSP PoC</h1>
<ul>
  <li><a href="/csp/strict/?bench=3">Strict CSP (no unsafe-eval, no wasm-unsafe-eval)</a></li>
  <li><a href="/csp/wasm-unsafe-eval/?bench=10">CSP with wasm-unsafe-eval</a></li>
  <li><a href="/csp/unsafe-eval/?bench=10">CSP with unsafe-eval (legacy)</a></li>
</ul>`;
      const bytes = Buffer.from(body, 'utf8');
      if (!withCommonHeaders(res)) return;
      try {
        res.writeHead(200, {
          'Content-Type': 'text/html; charset=utf-8',
          'Content-Length': String(bytes.byteLength),
        });
        res.end(bytes);
      } catch {
        try {
          res.destroy();
        } catch {
          // ignore
        }
      }
      return;
    }

    if (reqPath.startsWith('/csp/strict')) return await handleIndex(res, 'strict');
    if (reqPath.startsWith('/csp/wasm-unsafe-eval')) return await handleIndex(res, 'wasm-unsafe-eval');
    if (reqPath.startsWith('/csp/unsafe-eval')) return await handleIndex(res, 'unsafe-eval');

    const handled = handleStaticFile(reqPath, res);
    if (handled === null) {
      sendText(res, 400, 'Bad Request');
      return;
    }
    if (handled) return;

    sendText(res, 404, 'Not found');
  } catch (err) {
    // Avoid echoing internal error details back to the client.
    // eslint-disable-next-line no-console
    console.error(`poc-server: handler error: ${formatOneLineError(err, 512, 'Error')}`);
    // Defensive: avoid reading response state getters; `sendText` destroys on write failure.
    sendText(res, 500, 'Internal server error');
  }
});

server.listen(port, '127.0.0.1', () => {
  // eslint-disable-next-line no-console
  console.log(`[poc-server] listening on http://127.0.0.1:${port}`);
});
