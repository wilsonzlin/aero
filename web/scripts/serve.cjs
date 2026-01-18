const http = require('http');
const fs = require('fs');
const path = require('path');
// Playwright runs against this static server in CI. To keep tests lightweight we
// avoid a full Vite build/dev server, but we still need to execute TypeScript
// modules in the browser. Transpile `.ts`/`.tsx` on the fly using esbuild.
const esbuild = require('esbuild');
const { formatOneLineError, formatOneLineUtf8 } = require('../../scripts/_shared/text_one_line.cjs');

const MAX_REQUEST_URL_LEN = 8 * 1024;
const MAX_PATHNAME_LEN = 4 * 1024;
const MAX_ERROR_BODY_BYTES = 512;

function safeTextBody(message) {
  return formatOneLineUtf8(message, MAX_ERROR_BODY_BYTES) || 'Error';
}

function destroySafe(res) {
  try {
    res.destroy();
  } catch {
    // ignore
  }
}

function writeHeadSafe(res, statusCode, headers) {
  try {
    res.writeHead(statusCode, headers);
    return true;
  } catch {
    destroySafe(res);
    return false;
  }
}

function endSafe(res, body) {
  try {
    res.end(body);
    return true;
  } catch {
    destroySafe(res);
    return false;
  }
}

function sendText(res, statusCode, message) {
  const body = safeTextBody(message);
  if (
    !writeHeadSafe(res, statusCode, {
      'Content-Type': 'text/plain; charset=utf-8',
      'Cache-Control': 'no-store',
      'Content-Length': String(Buffer.byteLength(body)),
    })
  ) {
    return;
  }
  endSafe(res, body);
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
      sendText(res, 400, 'Bad Request');
      return;
    }
    if (rawUrl.length > MAX_REQUEST_URL_LEN) {
      sendText(res, 414, 'URI Too Long');
      return;
    }

    let url;
    try {
      // Base URL is only used to parse the request target; avoid tying it to the listen host.
      url = new URL(rawUrl, 'http://localhost');
    } catch {
      sendText(res, 400, 'Bad Request');
      return;
    }
    if (url.pathname.length > MAX_PATHNAME_LEN) {
      sendText(res, 414, 'URI Too Long');
      return;
    }

    let pathname;
    try {
      pathname = decodeURIComponent(url.pathname);
    } catch {
      sendText(res, 400, 'Bad Request');
      return;
    }
    if (pathname.length > MAX_PATHNAME_LEN) {
      sendText(res, 414, 'URI Too Long');
      return;
    }
    if (pathname.includes('\0')) {
      sendText(res, 400, 'Bad Request');
      return;
    }

    const rawPath = pathname === '/' ? '/index.html' : pathname;

    const absPath = resolveRequestPath(rawPath);
    if (!absPath) {
      sendText(res, 404, 'Not found');
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
      if (
        !writeHeadSafe(res, 200, {
          ...commonHeaders,
          'Content-Type': 'text/javascript; charset=utf-8',
        })
      ) {
        return;
      }
      endSafe(res, code);
      return;
    }

    const data = await fs.promises.readFile(absPath);
    if (
      !writeHeadSafe(res, 200, {
        ...commonHeaders,
        'Content-Type': contentTypeFor(absPath),
      })
    ) {
      return;
    }
    endSafe(res, data);
  })().catch((err) => {
    // Avoid echoing internal error details back to the client.
    // eslint-disable-next-line no-console
    console.error(`web serve: handler error: ${formatOneLineError(err, 512, "Error")}`);
    // Defensive: avoid relying on response state getters. Best-effort emit a 500.
    sendText(res, 500, 'Internal server error');
  });
});

server.listen(port, host, () => {
  console.log(`Serving ${rootDir} at http://${host}:${port}/`);
});

process.on('SIGTERM', () => server.close());
process.on('SIGINT', () => server.close());
