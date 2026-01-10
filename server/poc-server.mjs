import http from 'node:http';
import { readFile } from 'node:fs/promises';
import { createReadStream, existsSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const repoRoot = path.resolve(__dirname, '..');
const webPublicRoot = path.join(repoRoot, 'web', 'public');
const webDistRoot = path.join(repoRoot, 'web', 'dist');

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
  // COOP/COEP are required for SharedArrayBuffer + threads and (in Chrome) more accurate memory measurement APIs.
  res.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
  res.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
  res.setHeader('Cross-Origin-Resource-Policy', 'same-origin');
  res.setHeader('Origin-Agent-Cluster', '?1');
  res.setHeader('Cache-Control', 'no-store');
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

async function handleIndex(res, mode) {
  const html = await readFile(path.join(webPublicRoot, 'wasm-jit-csp', 'index.html'), 'utf8');
  withCommonHeaders(res);
  res.setHeader('Content-Security-Policy', cspHeader(mode));
  res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
  res.end(html);
}

function handleStaticFile(reqPath, res) {
  const decodedPath = decodeURIComponent(reqPath);

  // Serve built browser JS.
  if (decodedPath.startsWith('/dist/')) {
    const filePath = path.join(webDistRoot, decodedPath.slice('/dist/'.length));
    if (!isPathInside(webDistRoot, filePath) || !existsSync(filePath)) return false;
    withCommonHeaders(res);
    res.writeHead(200, { 'Content-Type': contentTypeFor(filePath) });
    createReadStream(filePath).pipe(res);
    return true;
  }

  // Serve static assets (including the precompiled mega-module).
  // `reqPath` always begins with `/`, which would otherwise cause `path.join(...)`
  // to treat it as absolute and ignore the root prefix.
  const publicRelPath = decodedPath.replace(/^\/+/, '');
  if (publicRelPath) {
    const filePath = path.join(webPublicRoot, publicRelPath);
    if (isPathInside(webPublicRoot, filePath) && existsSync(filePath)) {
      withCommonHeaders(res);
      res.writeHead(200, { 'Content-Type': contentTypeFor(filePath) });
      createReadStream(filePath).pipe(res);
      return true;
    }
  }

  return false;
}

const server = http.createServer(async (req, res) => {
  try {
    const url = new URL(req.url ?? '/', `http://${req.headers.host ?? 'localhost'}`);
    const reqPath = url.pathname;

    // CSP test entry points.
    if (reqPath === '/' || reqPath === '/index.html') {
      withCommonHeaders(res);
      res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
      res.end(`<!doctype html>
<meta charset="utf-8">
<title>Aero WASM JIT CSP PoC</title>
<h1>Aero WASM JIT CSP PoC</h1>
<ul>
  <li><a href="/csp/strict/?bench=3">Strict CSP (no unsafe-eval, no wasm-unsafe-eval)</a></li>
  <li><a href="/csp/wasm-unsafe-eval/?bench=10">CSP with wasm-unsafe-eval</a></li>
  <li><a href="/csp/unsafe-eval/?bench=10">CSP with unsafe-eval (legacy)</a></li>
</ul>`);
      return;
    }

    if (reqPath.startsWith('/csp/strict')) return await handleIndex(res, 'strict');
    if (reqPath.startsWith('/csp/wasm-unsafe-eval')) return await handleIndex(res, 'wasm-unsafe-eval');
    if (reqPath.startsWith('/csp/unsafe-eval')) return await handleIndex(res, 'unsafe-eval');

    if (handleStaticFile(reqPath, res)) return;

    withCommonHeaders(res);
    res.writeHead(404, { 'Content-Type': 'text/plain; charset=utf-8' });
    res.end('Not found');
  } catch (err) {
    withCommonHeaders(res);
    res.writeHead(500, { 'Content-Type': 'text/plain; charset=utf-8' });
    res.end(`Internal server error: ${err instanceof Error ? err.message : String(err)}`);
  }
});

server.listen(port, '127.0.0.1', () => {
  // eslint-disable-next-line no-console
  console.log(`[poc-server] listening on http://127.0.0.1:${port}`);
});
