const http = require('http');
const fs = require('fs');
const path = require('path');
// Playwright runs against this static server in CI. To keep tests lightweight we
// avoid a full Vite build/dev server, but we still need to execute TypeScript
// modules in the browser. Transpile `.ts`/`.tsx` on the fly using esbuild.
const esbuild = require('esbuild');

const MAX_REQUEST_URL_LEN = 8 * 1024;
const MAX_PATHNAME_LEN = 4 * 1024;
const MAX_ERROR_BODY_BYTES = 512;

function sanitizeOneLine(input) {
  let out = '';
  let pendingSpace = false;
  for (const ch of String(input ?? '')) {
    const code = ch.codePointAt(0) ?? 0;
    const forbidden = code <= 0x1f || code === 0x7f || code === 0x85 || code === 0x2028 || code === 0x2029;
    if (forbidden || /\s/u.test(ch)) {
      pendingSpace = out.length > 0;
      continue;
    }
    if (pendingSpace) {
      out += ' ';
      pendingSpace = false;
    }
    out += ch;
  }
  return out.trim();
}

function truncateUtf8(input, maxBytes) {
  if (!Number.isInteger(maxBytes) || maxBytes < 0) return '';
  const s = String(input ?? '');
  const buf = Buffer.from(s, 'utf8');
  if (buf.length <= maxBytes) return s;
  let cut = maxBytes;
  while (cut > 0 && (buf[cut] & 0xc0) === 0x80) cut -= 1;
  if (cut <= 0) return '';
  return buf.subarray(0, cut).toString('utf8');
}

function formatOneLineUtf8(input, maxBytes) {
  return truncateUtf8(sanitizeOneLine(input), maxBytes);
}

function safeTextBody(message) {
  return formatOneLineUtf8(message, MAX_ERROR_BODY_BYTES) || 'Error';
}

function parseArgs(argv) {
  const out = { host: '127.0.0.1', port: 4173 };
  for (let i = 2; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === '--port') out.port = Number(argv[++i]);
    else if (arg === '--host') out.host = argv[++i];
  }
  return out;
}

const { host, port } = parseArgs(process.argv);
const rootDir = path.join(__dirname, '..');
const coopCoepSetting = String(process.env.VITE_DISABLE_COOP_COEP ?? '').toLowerCase();
const coopCoepDisabled = coopCoepSetting === '1' || coopCoepSetting === 'true';

function contentTypeFor(filePath) {
  switch (path.extname(filePath)) {
    case '.html':
      return 'text/html; charset=utf-8';
    case '.js':
    case '.mjs':
      return 'text/javascript; charset=utf-8';
    case '.css':
      return 'text/css; charset=utf-8';
    case '.json':
      return 'application/json; charset=utf-8';
    case '.wasm':
      return 'application/wasm';
    default:
      return 'application/octet-stream';
  }
}

const tsCache = new Map();

function resolveExistingFile(absPath) {
  try {
    const stat = fs.statSync(absPath);
    return stat.isFile() ? absPath : null;
  } catch {
    return null;
  }
}

function resolveRequestPath(rawPath) {
  const resolvedPath = path.normalize(path.join(rootDir, rawPath));
  if (resolvedPath !== rootDir && !resolvedPath.startsWith(`${rootDir}${path.sep}`)) {
    return null;
  }

  const direct = resolveExistingFile(resolvedPath);
  if (direct) {
    return direct;
  }

  const ext = path.extname(resolvedPath);
  if (ext) {
    return null;
  }

  const candidates = [
    `${resolvedPath}.ts`,
    `${resolvedPath}.tsx`,
    `${resolvedPath}.js`,
    `${resolvedPath}.mjs`,
    path.join(resolvedPath, 'index.ts'),
    path.join(resolvedPath, 'index.tsx'),
    path.join(resolvedPath, 'index.js'),
    path.join(resolvedPath, 'index.mjs'),
  ];

  for (const candidate of candidates) {
    const found = resolveExistingFile(candidate);
    if (found) return found;
  }

  return null;
}

async function transpileTs(absPath) {
  const stat = await fs.promises.stat(absPath);
  const cached = tsCache.get(absPath);
  if (cached && cached.mtimeMs === stat.mtimeMs) {
    return cached.code;
  }

  const source = await fs.promises.readFile(absPath, 'utf8');
  const ext = path.extname(absPath);
  const loader = ext === '.tsx' ? 'tsx' : 'ts';

  const result = await esbuild.transform(source, {
    loader,
    format: 'esm',
    target: 'es2022',
    sourcemap: 'inline',
    sourcefile: absPath,
  });

  tsCache.set(absPath, { mtimeMs: stat.mtimeMs, code: result.code });
  return result.code;
}

const server = http.createServer((req, res) => {
  (async () => {
    const rawUrl = req.url ?? '/';
    if (typeof rawUrl !== 'string') {
      res.writeHead(400);
      res.end(safeTextBody('Bad Request'));
      return;
    }
    if (rawUrl.length > MAX_REQUEST_URL_LEN) {
      res.writeHead(414);
      res.end(safeTextBody('URI Too Long'));
      return;
    }

    let url;
    try {
      // Base URL is only used to parse the request target; avoid tying it to the listen host.
      url = new URL(rawUrl, 'http://localhost');
    } catch {
      res.writeHead(400);
      res.end(safeTextBody('Bad Request'));
      return;
    }
    if (url.pathname.length > MAX_PATHNAME_LEN) {
      res.writeHead(414);
      res.end(safeTextBody('URI Too Long'));
      return;
    }

    let pathname;
    try {
      pathname = decodeURIComponent(url.pathname);
    } catch {
      res.writeHead(400);
      res.end(safeTextBody('Bad Request'));
      return;
    }
    if (pathname.length > MAX_PATHNAME_LEN) {
      res.writeHead(414);
      res.end(safeTextBody('URI Too Long'));
      return;
    }
    if (pathname.includes('\0')) {
      res.writeHead(400);
      res.end(safeTextBody('Bad Request'));
      return;
    }

    const rawPath = pathname === '/' ? '/index.html' : pathname;

    const absPath = resolveRequestPath(rawPath);
    if (!absPath) {
      res.writeHead(404);
      res.end(safeTextBody('Not found'));
      return;
    }

    const commonHeaders = {
      // The real Aero project requires COOP/COEP for SharedArrayBuffer. Keeping these
      // headers here makes the demo behave like production from day one.
      ...(coopCoepDisabled
        ? {}
        : {
            'Cross-Origin-Opener-Policy': 'same-origin',
            'Cross-Origin-Embedder-Policy': 'require-corp',
          }),
    };

    if (absPath.endsWith('.ts') || absPath.endsWith('.tsx')) {
      const code = await transpileTs(absPath);
      res.writeHead(200, {
        ...commonHeaders,
        'Content-Type': 'text/javascript; charset=utf-8',
      });
      res.end(code);
      return;
    }

    const data = await fs.promises.readFile(absPath);
    res.writeHead(200, {
      ...commonHeaders,
      'Content-Type': contentTypeFor(absPath),
    });
    res.end(data);
  })().catch((err) => {
    res.writeHead(500);
    // Avoid echoing internal error details back to the client.
    // eslint-disable-next-line no-console
    console.error(err?.stack || err);
    res.end(safeTextBody("Internal server error"));
  });
});

server.listen(port, host, () => {
  console.log(`Serving ${rootDir} at http://${host}:${port}/`);
});

process.on('SIGTERM', () => server.close());
process.on('SIGINT', () => server.close());
